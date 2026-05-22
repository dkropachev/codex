use super::*;
use futures::StreamExt;

#[derive(Clone)]
pub(crate) struct CatalogRequestProcessor {
    pub(super) auth_manager: Arc<AuthManager>,
    pub(super) thread_manager: Arc<ThreadManager>,
    pub(super) config: Arc<Config>,
    pub(super) config_manager: ConfigManager,
    pub(super) workspace_settings_cache: Arc<workspace_settings::WorkspaceSettingsCache>,
}

const SKILLS_LIST_CWD_CONCURRENCY: usize = 5;

fn skills_to_info(
    skills: &[codex_core::skills::SkillMetadata],
    disabled_paths: &HashSet<AbsolutePathBuf>,
) -> Vec<codex_app_server_protocol::SkillMetadata> {
    skills
        .iter()
        .map(|skill| {
            let enabled = !disabled_paths.contains(&skill.path_to_skills_md);
            codex_app_server_protocol::SkillMetadata {
                name: skill.name.clone(),
                description: skill.description.clone(),
                short_description: skill.short_description.clone(),
                interface: skill.interface.clone().map(|interface| {
                    codex_app_server_protocol::SkillInterface {
                        display_name: interface.display_name,
                        short_description: interface.short_description,
                        icon_small: interface.icon_small,
                        icon_large: interface.icon_large,
                        brand_color: interface.brand_color,
                        default_prompt: interface.default_prompt,
                    }
                }),
                dependencies: skill.dependencies.clone().map(|dependencies| {
                    codex_app_server_protocol::SkillDependencies {
                        tools: dependencies
                            .tools
                            .into_iter()
                            .map(|tool| codex_app_server_protocol::SkillToolDependency {
                                r#type: tool.r#type,
                                value: tool.value,
                                description: tool.description,
                                transport: tool.transport,
                                command: tool.command,
                                url: tool.url,
                            })
                            .collect(),
                    }
                }),
                path: skill.path_to_skills_md.clone(),
                scope: skill.scope.into(),
                enabled,
            }
        })
        .collect()
}

fn hooks_to_info(hooks: &[codex_hooks::HookListEntry]) -> Vec<HookMetadata> {
    hooks
        .iter()
        .map(|hook| HookMetadata {
            key: hook.key.clone(),
            event_name: hook.event_name.into(),
            handler_type: hook.handler_type.into(),
            matcher: hook.matcher.clone(),
            command: hook.command.clone(),
            timeout_sec: hook.timeout_sec,
            status_message: hook.status_message.clone(),
            source_path: hook.source_path.clone(),
            source: hook.source.into(),
            plugin_id: hook.plugin_id.clone(),
            display_order: hook.display_order,
            enabled: hook.enabled,
            is_managed: hook.is_managed,
            current_hash: hook.current_hash.clone(),
            trust_status: hook.trust_status.into(),
        })
        .collect()
}

fn errors_to_info(
    errors: &[codex_core::skills::SkillError],
) -> Vec<codex_app_server_protocol::SkillErrorInfo> {
    errors
        .iter()
        .map(|err| codex_app_server_protocol::SkillErrorInfo {
            path: err.path.to_path_buf(),
            message: err.message.clone(),
        })
        .collect()
}

impl CatalogRequestProcessor {
    pub(crate) fn new(
        auth_manager: Arc<AuthManager>,
        thread_manager: Arc<ThreadManager>,
        config: Arc<Config>,
        config_manager: ConfigManager,
        workspace_settings_cache: Arc<workspace_settings::WorkspaceSettingsCache>,
    ) -> Self {
        Self {
            auth_manager,
            thread_manager,
            config,
            config_manager,
            workspace_settings_cache,
        }
    }

    pub(crate) async fn skills_list(
        &self,
        params: SkillsListParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.skills_list_response(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn hooks_list(
        &self,
        params: HooksListParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.hooks_list_response(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn api_catalog_read(
        &self,
        params: ApiCatalogReadParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.api_catalog_read_response(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn skills_config_write(
        &self,
        params: SkillsConfigWriteParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.skills_config_write_response_inner(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn model_list(
        &self,
        params: ModelListParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        Self::list_models(self.thread_manager.clone(), params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn experimental_feature_list(
        &self,
        params: ExperimentalFeatureListParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.experimental_feature_list_response(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn collaboration_mode_list(
        &self,
        params: CollaborationModeListParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        Self::list_collaboration_modes(self.thread_manager.clone(), params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn mock_experimental_method(
        &self,
        params: MockExperimentalMethodParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.mock_experimental_method_inner(params)
            .await
            .map(|response| Some(response.into()))
    }

    async fn resolve_cwd_config(
        &self,
        cwd: &Path,
    ) -> Result<(AbsolutePathBuf, ConfigLayerStack), String> {
        let cwd_abs =
            AbsolutePathBuf::relative_to_current_dir(cwd).map_err(|err| err.to_string())?;
        let config_layer_stack = self
            .config_manager
            .load_config_layers_for_cwd(cwd_abs.clone())
            .await
            .map_err(|err| err.to_string())?;

        Ok((cwd_abs, config_layer_stack))
    }

    async fn load_latest_config(
        &self,
        fallback_cwd: Option<PathBuf>,
    ) -> Result<Config, JSONRPCErrorError> {
        self.config_manager
            .load_latest_config(fallback_cwd)
            .await
            .map_err(|err| internal_error(format!("failed to reload config: {err}")))
    }

    async fn api_catalog_read_response(
        &self,
        params: ApiCatalogReadParams,
    ) -> Result<ApiCatalogReadResponse, JSONRPCErrorError> {
        let include = params.include;
        let app_server_methods =
            if api_catalog_includes(&include, ApiCatalogSection::AppServerMethods) {
                api_catalog_methods()
            } else {
                Vec::new()
            };
        let mcp_servers = if api_catalog_includes(&include, ApiCatalogSection::McpServers) {
            self.api_catalog_mcp_servers(params.mcp_detail).await?
        } else {
            Vec::new()
        };
        let built_in_tools = if api_catalog_includes(&include, ApiCatalogSection::BuiltInTools) {
            codex_app_server_protocol::built_in_api_catalog_tools()
        } else {
            Vec::new()
        };
        let workflow_runtime = if api_catalog_includes(&include, ApiCatalogSection::WorkflowRuntime)
        {
            codex_app_server_protocol::workflow_runtime_api_catalog()
        } else {
            codex_app_server_protocol::ApiCatalogWorkflowRuntime {
                package: String::new(),
                import_specifier: String::new(),
                symbols: Vec::new(),
            }
        };
        let workflows = if api_catalog_includes(&include, ApiCatalogSection::Workflows) {
            self.api_catalog_workflows()?
        } else {
            Vec::new()
        };

        Ok(ApiCatalogReadResponse {
            schema_version: 1,
            generated_at: Utc::now().timestamp(),
            app_server_methods,
            mcp_servers,
            built_in_tools,
            workflow_runtime,
            workflows,
        })
    }

    fn api_catalog_workflows(
        &self,
    ) -> Result<Vec<codex_app_server_protocol::WorkflowSummary>, JSONRPCErrorError> {
        codex_workflows::discover_workflows(
            self.config.codex_home.as_path(),
            self.config.cwd.as_path(),
            &self.config.workflows,
        )
        .map(|workflows| {
            workflows
                .into_iter()
                .map(api_catalog_workflow_to_info)
                .collect()
        })
        .map_err(|err| internal_error(format!("failed to discover workflows: {err}")))
    }

    async fn api_catalog_mcp_servers(
        &self,
        detail: Option<McpServerStatusDetail>,
    ) -> Result<Vec<McpServerStatus>, JSONRPCErrorError> {
        let config = self.load_latest_config(/*fallback_cwd*/ None).await?;
        let mcp_config = config
            .to_mcp_config(self.thread_manager.plugins_manager().as_ref())
            .await;
        let auth = self.auth_manager.auth().await;
        let environment_manager = self.thread_manager.environment_manager();
        let runtime_environment = match environment_manager.default_environment() {
            Some(environment) => McpRuntimeEnvironment::new(environment, config.cwd.to_path_buf()),
            None => McpRuntimeEnvironment::new(
                environment_manager.local_environment(),
                config.cwd.to_path_buf(),
            ),
        };

        let response = McpRequestProcessor::list_mcp_server_status_response(
            "apiCatalog/read".to_string(),
            ListMcpServerStatusParams {
                cursor: None,
                limit: None,
                detail,
            },
            config,
            mcp_config,
            auth,
            runtime_environment,
        )
        .await?;

        Ok(response.data)
    }

    async fn workspace_codex_plugins_enabled(
        &self,
        config: &Config,
        auth: Option<&CodexAuth>,
    ) -> bool {
        match workspace_settings::codex_plugins_enabled_for_workspace(
            config,
            auth,
            Some(&self.workspace_settings_cache),
        )
        .await
        {
            Ok(enabled) => enabled,
            Err(err) => {
                warn!(
                    "failed to fetch workspace Codex plugins setting; allowing Codex plugins: {err:#}"
                );
                true
            }
        }
    }

    async fn list_models(
        thread_manager: Arc<ThreadManager>,
        params: ModelListParams,
    ) -> Result<ModelListResponse, JSONRPCErrorError> {
        let ModelListParams {
            limit,
            cursor,
            include_hidden,
        } = params;
        let models = supported_models(thread_manager, include_hidden.unwrap_or(false)).await;
        let total = models.len();

        if total == 0 {
            return Ok(ModelListResponse {
                data: Vec::new(),
                next_cursor: None,
            });
        }

        let effective_limit = limit.unwrap_or(total as u32).max(1) as usize;
        let effective_limit = effective_limit.min(total);
        let start = match cursor {
            Some(cursor) => cursor
                .parse::<usize>()
                .map_err(|_| invalid_request(format!("invalid cursor: {cursor}")))?,
            None => 0,
        };

        if start > total {
            return Err(invalid_request(format!(
                "cursor {start} exceeds total models {total}"
            )));
        }

        let end = start.saturating_add(effective_limit).min(total);
        let items = models[start..end].to_vec();
        let next_cursor = if end < total {
            Some(end.to_string())
        } else {
            None
        };
        Ok(ModelListResponse {
            data: items,
            next_cursor,
        })
    }

    async fn list_collaboration_modes(
        thread_manager: Arc<ThreadManager>,
        params: CollaborationModeListParams,
    ) -> Result<CollaborationModeListResponse, JSONRPCErrorError> {
        let CollaborationModeListParams {} = params;
        let items = thread_manager
            .list_collaboration_modes()
            .into_iter()
            .map(Into::into)
            .collect();
        let response = CollaborationModeListResponse { data: items };
        Ok(response)
    }

    async fn experimental_feature_list_response(
        &self,
        params: ExperimentalFeatureListParams,
    ) -> Result<ExperimentalFeatureListResponse, JSONRPCErrorError> {
        let ExperimentalFeatureListParams { cursor, limit } = params;
        let config = self.load_latest_config(/*fallback_cwd*/ None).await?;
        let auth = self.auth_manager.auth().await;
        let workspace_codex_plugins_enabled = self
            .workspace_codex_plugins_enabled(&config, auth.as_ref())
            .await;

        let data = FEATURES
            .iter()
            .map(|spec| {
                let (stage, display_name, description, announcement) = match spec.stage {
                    Stage::Experimental {
                        name,
                        menu_description,
                        announcement,
                    } => (
                        ApiExperimentalFeatureStage::Beta,
                        Some(name.to_string()),
                        Some(menu_description.to_string()),
                        Some(announcement.to_string()),
                    ),
                    Stage::UnderDevelopment => (
                        ApiExperimentalFeatureStage::UnderDevelopment,
                        None,
                        None,
                        None,
                    ),
                    Stage::Stable => (ApiExperimentalFeatureStage::Stable, None, None, None),
                    Stage::Deprecated => {
                        (ApiExperimentalFeatureStage::Deprecated, None, None, None)
                    }
                    Stage::Removed => (ApiExperimentalFeatureStage::Removed, None, None, None),
                };

                ApiExperimentalFeature {
                    name: spec.key.to_string(),
                    stage,
                    display_name,
                    description,
                    announcement,
                    enabled: config.features.enabled(spec.id)
                        && (workspace_codex_plugins_enabled
                            || !matches!(spec.id, Feature::Apps | Feature::Plugins)),
                    default_enabled: spec.default_enabled,
                }
            })
            .collect::<Vec<_>>();

        let total = data.len();
        if total == 0 {
            return Ok(ExperimentalFeatureListResponse {
                data: Vec::new(),
                next_cursor: None,
            });
        }

        // Clamp to 1 so limit=0 cannot return a non-advancing page.
        let effective_limit = limit.unwrap_or(total as u32).max(1) as usize;
        let effective_limit = effective_limit.min(total);
        let start = match cursor {
            Some(cursor) => match cursor.parse::<usize>() {
                Ok(idx) => idx,
                Err(_) => return Err(invalid_request(format!("invalid cursor: {cursor}"))),
            },
            None => 0,
        };

        if start > total {
            return Err(invalid_request(format!(
                "cursor {start} exceeds total feature flags {total}"
            )));
        }

        let end = start.saturating_add(effective_limit).min(total);
        let data = data[start..end].to_vec();
        let next_cursor = if end < total {
            Some(end.to_string())
        } else {
            None
        };

        Ok(ExperimentalFeatureListResponse { data, next_cursor })
    }

    async fn mock_experimental_method_inner(
        &self,
        params: MockExperimentalMethodParams,
    ) -> Result<MockExperimentalMethodResponse, JSONRPCErrorError> {
        let MockExperimentalMethodParams { value } = params;
        let response = MockExperimentalMethodResponse { echoed: value };
        Ok(response)
    }

    async fn skills_list_response(
        &self,
        params: SkillsListParams,
    ) -> Result<SkillsListResponse, JSONRPCErrorError> {
        let SkillsListParams {
            cwds,
            force_reload,
            per_cwd_extra_user_roots,
        } = params;
        let cwds = if cwds.is_empty() {
            vec![self.config.cwd.to_path_buf()]
        } else {
            cwds
        };
        let cwd_set: HashSet<PathBuf> = cwds.iter().cloned().collect();

        let mut extra_roots_by_cwd: HashMap<PathBuf, Vec<AbsolutePathBuf>> = HashMap::new();
        for entry in per_cwd_extra_user_roots.unwrap_or_default() {
            if !cwd_set.contains(&entry.cwd) {
                warn!(
                    cwd = %entry.cwd.display(),
                    "ignoring per-cwd extra roots for cwd not present in skills/list cwds"
                );
                continue;
            }

            let mut valid_extra_roots = Vec::new();
            for root in entry.extra_user_roots {
                let root =
                    AbsolutePathBuf::from_absolute_path_checked(root.as_path()).map_err(|_| {
                        invalid_request(format!(
                            "skills/list perCwdExtraUserRoots extraUserRoots paths must be absolute: {}",
                            root.display()
                        ))
                    })?;
                valid_extra_roots.push(root);
            }
            extra_roots_by_cwd
                .entry(entry.cwd)
                .or_default()
                .extend(valid_extra_roots);
        }

        let config = self.load_latest_config(/*fallback_cwd*/ None).await?;
        let auth = self.auth_manager.auth().await;
        let workspace_codex_plugins_enabled = self
            .workspace_codex_plugins_enabled(&config, auth.as_ref())
            .await;
        let skills_manager = self.thread_manager.skills_manager();
        let plugins_manager = self.thread_manager.plugins_manager();
        let fs = self
            .thread_manager
            .environment_manager()
            .default_environment()
            .map(|environment| environment.get_filesystem());
        let mut data = futures::stream::iter(cwds.into_iter().enumerate())
            .map(|(index, cwd)| {
                let config = &config;
                let extra_roots_by_cwd = &extra_roots_by_cwd;
                let fs = fs.clone();
                let plugins_manager = &plugins_manager;
                let skills_manager = &skills_manager;
                async move {
                    let (cwd_abs, config_layer_stack) = match self.resolve_cwd_config(&cwd).await {
                        Ok(resolved) => resolved,
                        Err(message) => {
                            let error_path = cwd.clone();
                            return (
                                index,
                                codex_app_server_protocol::SkillsListEntry {
                                    cwd,
                                    skills: Vec::new(),
                                    errors: vec![codex_app_server_protocol::SkillErrorInfo {
                                        path: error_path,
                                        message,
                                    }],
                                },
                            );
                        }
                    };
                    let extra_roots = extra_roots_by_cwd
                        .get(&cwd)
                        .map_or(&[][..], std::vec::Vec::as_slice);
                    let effective_skill_roots = if workspace_codex_plugins_enabled {
                        let plugins_input = config.plugins_config_input();
                        plugins_manager
                            .effective_skill_roots_for_layer_stack(
                                &config_layer_stack,
                                &plugins_input,
                            )
                            .await
                    } else {
                        Vec::new()
                    };
                    let skills_input = codex_core::skills::SkillsLoadInput::new(
                        cwd_abs.clone(),
                        effective_skill_roots,
                        config_layer_stack,
                        config.bundled_skills_enabled(),
                    );
                    let outcome = skills_manager
                        .skills_for_cwd_with_extra_user_roots(
                            &skills_input,
                            force_reload,
                            extra_roots,
                            fs,
                        )
                        .await;
                    let errors = errors_to_info(&outcome.errors);
                    let skills = skills_to_info(&outcome.skills, &outcome.disabled_paths);
                    (
                        index,
                        codex_app_server_protocol::SkillsListEntry {
                            cwd,
                            skills,
                            errors,
                        },
                    )
                }
            })
            .buffer_unordered(SKILLS_LIST_CWD_CONCURRENCY)
            .collect::<Vec<_>>()
            .await;
        data.sort_unstable_by_key(|(index, _)| *index);
        let data = data.into_iter().map(|(_, entry)| entry).collect();
        Ok(SkillsListResponse { data })
    }

    /// Handle `hooks/list` by resolving hooks for each requested cwd.
    async fn hooks_list_response(
        &self,
        params: HooksListParams,
    ) -> Result<HooksListResponse, JSONRPCErrorError> {
        let HooksListParams { cwds } = params;
        let cwds = if cwds.is_empty() {
            vec![self.config.cwd.to_path_buf()]
        } else {
            cwds
        };

        let auth = self.auth_manager.auth().await;
        let plugins_manager = self.thread_manager.plugins_manager();
        let mut data = Vec::new();
        for cwd in cwds {
            let config = match self
                .config_manager
                .load_for_cwd(
                    /*request_overrides*/ None,
                    ConfigOverrides::default(),
                    Some(cwd.clone()),
                )
                .await
            {
                Ok(config) => config,
                Err(err) => {
                    let error_path = cwd.clone();
                    data.push(codex_app_server_protocol::HooksListEntry {
                        cwd,
                        hooks: Vec::new(),
                        warnings: Vec::new(),
                        errors: vec![codex_app_server_protocol::HookErrorInfo {
                            path: error_path,
                            message: err.to_string(),
                        }],
                    });
                    continue;
                }
            };
            let workspace_codex_plugins_enabled = self
                .workspace_codex_plugins_enabled(&config, auth.as_ref())
                .await;
            let plugins_enabled =
                config.features.enabled(Feature::Plugins) && workspace_codex_plugins_enabled;
            let plugin_outcome = if plugins_enabled && config.features.enabled(Feature::PluginHooks)
            {
                let plugins_input = config.plugins_config_input();
                plugins_manager
                    .plugins_for_layer_stack(
                        &config.config_layer_stack,
                        &plugins_input,
                        /*plugin_hooks_feature_enabled*/ true,
                    )
                    .await
            } else {
                PluginLoadOutcome::default()
            };
            let hooks = codex_hooks::list_hooks(codex_hooks::HooksConfig {
                feature_enabled: config.features.enabled(Feature::CodexHooks),
                config_layer_stack: Some(config.config_layer_stack),
                plugin_hook_sources: plugin_outcome.effective_plugin_hook_sources(),
                plugin_hook_load_warnings: plugin_outcome.effective_plugin_hook_warnings(),
                ..Default::default()
            });
            data.push(codex_app_server_protocol::HooksListEntry {
                cwd,
                hooks: hooks_to_info(&hooks.hooks),
                warnings: hooks.warnings,
                errors: Vec::new(),
            });
        }
        Ok(HooksListResponse { data })
    }

    async fn skills_config_write_response_inner(
        &self,
        params: SkillsConfigWriteParams,
    ) -> Result<SkillsConfigWriteResponse, JSONRPCErrorError> {
        let SkillsConfigWriteParams {
            path,
            name,
            enabled,
        } = params;
        let edit = match (path, name) {
            (Some(path), None) => ConfigEdit::SetSkillConfig {
                path: path.into_path_buf(),
                enabled,
            },
            (None, Some(name)) if !name.trim().is_empty() => {
                ConfigEdit::SetSkillConfigByName { name, enabled }
            }
            _ => {
                return Err(invalid_params(
                    "skills/config/write requires exactly one of path or name",
                ));
            }
        };
        let edits = vec![edit];
        ConfigEditsBuilder::new(&self.config.codex_home)
            .with_edits(edits)
            .apply()
            .await
            .map(|()| {
                self.thread_manager.plugins_manager().clear_cache();
                self.thread_manager.skills_manager().clear_cache();
                SkillsConfigWriteResponse {
                    effective_enabled: enabled,
                }
            })
            .map_err(|err| internal_error(format!("failed to update skill settings: {err}")))
    }
}

fn api_catalog_includes(
    include: &Option<Vec<ApiCatalogSection>>,
    section: ApiCatalogSection,
) -> bool {
    match include {
        Some(sections) => sections.contains(&section),
        None => true,
    }
}

fn api_catalog_methods() -> Vec<ApiCatalogMethod> {
    codex_app_server_protocol::client_method_infos()
        .into_iter()
        .map(|method| ApiCatalogMethod {
            method: method.method,
            params_type: method.params_type.to_string(),
            response_type: method.response_type.to_string(),
            experimental: method.experimental,
            description: method.description,
        })
        .collect()
}

fn api_catalog_workflow_to_info(
    workflow: codex_workflows::WorkflowSummary,
) -> codex_app_server_protocol::WorkflowSummary {
    codex_app_server_protocol::WorkflowSummary {
        id: workflow.id,
        command: workflow.command,
        title: workflow.title,
        user_description: workflow.user_description,
        search_terms: workflow.search_terms,
        command_option_hints: workflow
            .command_option_hints
            .into_iter()
            .map(
                |hint| codex_app_server_protocol::WorkflowCommandOptionHint {
                    display: hint.display,
                    description: hint.description,
                },
            )
            .collect(),
        root_label: workflow.root_label,
        root_kind: match workflow.root_kind {
            codex_workflows::WorkflowRootKind::Global => {
                codex_app_server_protocol::WorkflowRootKind::Global
            }
            codex_workflows::WorkflowRootKind::Project => {
                codex_app_server_protocol::WorkflowRootKind::Project
            }
            codex_workflows::WorkflowRootKind::SearchPath => {
                codex_app_server_protocol::WorkflowRootKind::SearchPath
            }
        },
        root_path: workflow.root_path,
        path: workflow.path,
        workflow_yaml_path: workflow.workflow_yaml_path,
        mention_target: workflow.mention_target,
        validation: codex_app_server_protocol::WorkflowValidationInfo {
            status: match workflow.validation.status {
                codex_workflows::WorkflowValidationStatus::Valid => {
                    codex_app_server_protocol::WorkflowValidationStatus::Valid
                }
                codex_workflows::WorkflowValidationStatus::Invalid => {
                    codex_app_server_protocol::WorkflowValidationStatus::Invalid
                }
            },
            messages: workflow.validation.messages,
        },
        repair_mode: workflow.repair_mode,
    }
}

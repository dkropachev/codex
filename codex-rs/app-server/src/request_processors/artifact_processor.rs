use std::sync::Arc;

use codex_app_server_protocol::ArtifactCacheDeleteParams;
use codex_app_server_protocol::ArtifactCacheDeleteResponse;
use codex_app_server_protocol::ArtifactCacheEntryInfo;
use codex_app_server_protocol::ArtifactCacheReadParams;
use codex_app_server_protocol::ArtifactCacheReadResponse;
use codex_app_server_protocol::ArtifactCacheWriteParams;
use codex_app_server_protocol::ArtifactCacheWriteResponse;
use codex_app_server_protocol::ArtifactFileFindParams;
use codex_app_server_protocol::ArtifactFileFindResponse;
use codex_app_server_protocol::ArtifactFileIndexParams;
use codex_app_server_protocol::ArtifactFileIndexResponse;
use codex_app_server_protocol::ArtifactFileInfo;
use codex_app_server_protocol::ArtifactFileMatchInfo;
use codex_app_server_protocol::ArtifactStateHitParams;
use codex_app_server_protocol::ArtifactStateHitResponse;
use codex_app_server_protocol::ArtifactStateInfo;
use codex_app_server_protocol::ArtifactStateListParams;
use codex_app_server_protocol::ArtifactStateListResponse;
use codex_app_server_protocol::ArtifactStatePruneParams;
use codex_app_server_protocol::ArtifactStatePruneResponse;
use codex_app_server_protocol::ArtifactStateReadParams;
use codex_app_server_protocol::ArtifactStateReadResponse;
use codex_app_server_protocol::ArtifactStateRegisterParams;
use codex_app_server_protocol::ArtifactStateRegisterResponse;
use codex_app_server_protocol::ClientResponsePayload;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_artifactory::ArtifactFile;
use codex_artifactory::ArtifactSource;
use codex_artifactory::ArtifactState;
use codex_artifactory::Artifactory;
use codex_artifactory::PruneOptions;
use codex_artifactory::StateRegistration;
use codex_core::config::Config;
use serde_json::Value as JsonValue;

use crate::error_code::internal_error;

#[derive(Clone)]
pub(crate) struct ArtifactRequestProcessor {
    config: Arc<Config>,
}

impl ArtifactRequestProcessor {
    pub(crate) fn new(config: Arc<Config>) -> Self {
        Self { config }
    }

    pub(crate) async fn state_register(
        &self,
        params: ArtifactStateRegisterParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let mut store = self.open_store()?;
        let state = store
            .register_state(&StateRegistration {
                namespace: params.namespace,
                scope_key: params.scope_key,
                source_key: params.source_key,
                state_dir: params.state_dir,
                sources: params
                    .sources
                    .into_iter()
                    .map(|source| ArtifactSource::new(source.path, source.kind, source.sha256))
                    .collect(),
                metadata_json: serde_json::to_string(&params.metadata).map_err(|err| {
                    internal_error(format!("failed to encode artifact metadata: {err}"))
                })?,
            })
            .map_err(|err| internal_error(format!("failed to register artifact state: {err}")))?;
        Ok(Some(
            ArtifactStateRegisterResponse {
                state: state_to_api(state),
            }
            .into(),
        ))
    }

    pub(crate) async fn state_read(
        &self,
        params: ArtifactStateReadParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let store = self.open_store()?;
        let state = store
            .state_by_keys(&params.namespace, &params.scope_key, &params.source_key)
            .map_err(|err| internal_error(format!("failed to read artifact state: {err}")))?;
        Ok(Some(
            ArtifactStateReadResponse {
                state: state.map(state_to_api),
            }
            .into(),
        ))
    }

    pub(crate) async fn state_list(
        &self,
        params: ArtifactStateListParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let store = self.open_store()?;
        let states = store
            .states_for_scope(&params.namespace, &params.scope_key)
            .map_err(|err| internal_error(format!("failed to list artifact states: {err}")))?;
        Ok(Some(
            ArtifactStateListResponse {
                states: states.into_iter().map(state_to_api).collect(),
            }
            .into(),
        ))
    }

    pub(crate) async fn state_hit(
        &self,
        params: ArtifactStateHitParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let store = self.open_store()?;
        store
            .record_state_hit_by_dir(&params.namespace, &params.state_dir)
            .map_err(|err| internal_error(format!("failed to record artifact state hit: {err}")))?;
        Ok(Some(ArtifactStateHitResponse {}.into()))
    }

    pub(crate) async fn state_prune(
        &self,
        params: ArtifactStatePruneParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let store = self.open_store()?;
        let pruned = store
            .prune_stale_states(
                &params.namespace,
                PruneOptions::new(params.retention_secs, params.throttle_secs),
            )
            .map_err(|err| internal_error(format!("failed to prune artifact states: {err}")))?;
        let pruned = u32::try_from(pruned)
            .map_err(|_| internal_error("failed to convert pruned artifact count"))?;
        Ok(Some(ArtifactStatePruneResponse { pruned }.into()))
    }

    pub(crate) async fn file_index(
        &self,
        params: ArtifactFileIndexParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let store = self.open_store()?;
        store
            .index_file(&params.namespace, &params.state_dir, &params.relative_path)
            .map_err(|err| internal_error(format!("failed to index artifact file: {err}")))?;
        let Some((_, file)) = store
            .find_file(&params.namespace, &params.relative_path)
            .map_err(|err| {
                internal_error(format!("failed to read indexed artifact file: {err}"))
            })?
        else {
            return Err(internal_error(
                "indexed artifact file was not found after indexing",
            ));
        };
        Ok(Some(
            ArtifactFileIndexResponse {
                file: file_to_api(file),
            }
            .into(),
        ))
    }

    pub(crate) async fn file_find(
        &self,
        params: ArtifactFileFindParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let store = self.open_store()?;
        let entry = store
            .find_file(&params.namespace, &params.relative_path)
            .map_err(|err| internal_error(format!("failed to find artifact file: {err}")))?;
        Ok(Some(
            ArtifactFileFindResponse {
                entry: entry.map(|(state, file)| ArtifactFileMatchInfo {
                    state: state_to_api(state),
                    file: file_to_api(file),
                }),
            }
            .into(),
        ))
    }

    pub(crate) async fn cache_read(
        &self,
        params: ArtifactCacheReadParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let store = self.open_store()?;
        let entry = store
            .cache_entry(&params.namespace, &params.key)
            .map_err(|err| internal_error(format!("failed to read artifact cache entry: {err}")))?;
        Ok(Some(
            ArtifactCacheReadResponse {
                entry: entry.map(cache_entry_to_api),
            }
            .into(),
        ))
    }

    pub(crate) async fn cache_write(
        &self,
        params: ArtifactCacheWriteParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let store = self.open_store()?;
        let metadata = serde_json::to_string(&params.metadata).map_err(|err| {
            internal_error(format!("failed to encode artifact cache metadata: {err}"))
        })?;
        store
            .put_cache_entry(
                &params.namespace,
                &params.key,
                &params.artifact_id,
                &params.status,
                &metadata,
            )
            .map_err(|err| {
                internal_error(format!("failed to write artifact cache entry: {err}"))
            })?;
        let Some(entry) = store
            .cache_entry(&params.namespace, &params.key)
            .map_err(|err| internal_error(format!("failed to read artifact cache entry: {err}")))?
        else {
            return Err(internal_error(
                "artifact cache entry was not found after writing",
            ));
        };
        Ok(Some(
            ArtifactCacheWriteResponse {
                entry: cache_entry_to_api(entry),
            }
            .into(),
        ))
    }

    pub(crate) async fn cache_delete(
        &self,
        params: ArtifactCacheDeleteParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let store = self.open_store()?;
        store
            .delete_cache_entry(&params.namespace, &params.key)
            .map_err(|err| {
                internal_error(format!("failed to delete artifact cache entry: {err}"))
            })?;
        Ok(Some(ArtifactCacheDeleteResponse {}.into()))
    }

    fn open_store(&self) -> Result<Artifactory, JSONRPCErrorError> {
        Artifactory::open(self.config.codex_home.as_path())
            .map_err(|err| internal_error(format!("failed to open artifactory store: {err}")))
    }
}

fn state_to_api(state: ArtifactState) -> ArtifactStateInfo {
    ArtifactStateInfo {
        id: state.id,
        namespace: state.namespace,
        scope_key: state.scope_key,
        source_key: state.source_key,
        state_dir: state.state_dir,
        metadata: parse_metadata_json(&state.metadata_json),
        created_at_unix_sec: state.created_at_unix_sec,
        updated_at_unix_sec: state.updated_at_unix_sec,
        last_hit_at_unix_sec: state.last_hit_at_unix_sec,
    }
}

fn file_to_api(file: ArtifactFile) -> ArtifactFileInfo {
    ArtifactFileInfo {
        state_id: file.state_id,
        relative_path: file.relative_path,
        size_bytes: file.size_bytes,
        sha256: file.sha256,
        updated_at_unix_sec: file.updated_at_unix_sec,
    }
}

fn cache_entry_to_api(entry: codex_artifactory::CacheEntry) -> ArtifactCacheEntryInfo {
    ArtifactCacheEntryInfo {
        namespace: entry.namespace,
        key: entry.key,
        artifact_id: entry.artifact_id,
        status: entry.status,
        metadata: parse_metadata_json(&entry.metadata_json),
        created_at_unix_sec: entry.created_at_unix_sec,
        updated_at_unix_sec: entry.updated_at_unix_sec,
        last_hit_at_unix_sec: entry.last_hit_at_unix_sec,
    }
}

fn parse_metadata_json(value: &str) -> JsonValue {
    serde_json::from_str(value).unwrap_or_else(|_| JsonValue::String(value.to_string()))
}

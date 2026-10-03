//! Prepares and commits settings carried by turn-input submissions.

use super::super::session::Session;
use super::super::session::SessionConfiguration;
use super::super::session::SessionSettingsUpdate;
use super::super::thread_settings;
use super::super::turn_context::NewTurnContextOptions;
use super::super::turn_context::TurnContext;
use codex_protocol::config_types::ModeKind;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::turn_input::AppServerClientInfo;
use codex_protocol::turn_input::TurnStartOptions;
use serde_json::Value;
use std::sync::Arc;

#[cfg(test)]
#[path = "turn_input_settings_tests.rs"]
mod tests;

/// Why input is starting a turn; shared by admission and input delivery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TurnStartKind {
    User,
    Automatic,
    Recovery,
}

impl TurnStartKind {
    pub(super) fn permits_mode(self, mode: ModeKind) -> bool {
        match self {
            Self::User | Self::Recovery => true,
            Self::Automatic => mode != ModeKind::Plan,
        }
    }

    /// Automatic work may neither leave an existing Plan mode nor enter it.
    fn permits_settings(
        self,
        current: &SessionConfiguration,
        proposed: &SessionConfiguration,
    ) -> bool {
        self.permits_mode(current.step_settings.collaboration_mode.mode)
            && self.permits_mode(proposed.step_settings.collaboration_mode.mode)
    }
}

/// Thread settings and start-only options prepared before Core knows whether
/// turn input starts or steers.
///
/// Thread settings are validated up front but only applied after Core accepts
/// the input. Start-only options are only consumed by `apply_started`.
pub(super) struct PreparedTurnInputSettings {
    thread_settings_update: Option<SessionSettingsUpdate>,
    pub(super) start_options: TurnStartOptions,
    app_server_client_info: Option<AppServerClientInfo>,
}

impl PreparedTurnInputSettings {
    /// Validates turn-input settings without applying them so rejected input
    /// leaves the thread unchanged.
    pub(super) async fn prepare(
        session: &Session,
        thread_settings: ThreadSettingsOverrides,
        start_options: TurnStartOptions,
    ) -> CodexResult<Self> {
        let thread_settings_update = if thread_settings == ThreadSettingsOverrides::default() {
            None
        } else {
            let updates = thread_settings::prepare_update(thread_settings);
            session
                .preview_settings(&updates)
                .await
                .map_err(|error| CodexErr::InvalidRequest(error.to_string()))?;
            Some(updates)
        };
        Ok(Self {
            thread_settings_update,
            start_options,
            app_server_client_info: None,
        })
    }

    pub(super) fn with_app_server_client_info(
        mut self,
        app_server_client_info: Option<AppServerClientInfo>,
    ) -> Self {
        self.app_server_client_info = app_server_client_info;
        self
    }

    pub(super) fn required_active_final_output_json_schema(&self) -> Option<&Value> {
        self.start_options.final_output_json_schema.as_ref()
    }

    /// Applies persistent settings and start-only options before creating a
    /// new turn context. Returns `None` if admission rejects the candidate,
    /// without committing its settings.
    pub(super) async fn apply_started(
        self,
        session: &Arc<Session>,
        submission_id: String,
        kind: TurnStartKind,
    ) -> CodexResult<Option<Arc<TurnContext>>> {
        let TurnStartOptions {
            turn_trigger,
            final_output_json_schema,
            service_tier,
            parent_turn_id,
            root_turn_id,
            cyber_access_program,
        } = self.start_options;
        let emit_thread_settings_applied = self.thread_settings_update.is_some();
        let _settings_guard = if emit_thread_settings_applied {
            Some(thread_settings::acquire_persistence_lock(session).await)
        } else {
            None
        };
        let mut updates = self.thread_settings_update.unwrap_or_default();
        updates.service_tier_for_turn = service_tier;
        if let Some(client_info) = &self.app_server_client_info {
            updates.app_server_client_name = client_info.name.clone();
            updates.app_server_client_version = client_info.version.clone();
        }

        let options = NewTurnContextOptions {
            final_output_json_schema,
            cyber_access_program,
        };
        let turn_context = match kind {
            TurnStartKind::User | TurnStartKind::Recovery => Some(
                session
                    .new_turn_with_sub_id(submission_id.clone(), updates, options)
                    .await?,
            ),
            TurnStartKind::Automatic => {
                session
                    .new_turn_with_sub_id_if(
                        submission_id.clone(),
                        updates,
                        options,
                        |current, proposed| kind.permits_settings(current, proposed),
                    )
                    .await?
            }
        };
        let Some((turn_context, settings_snapshot)) = turn_context else {
            return Ok(None);
        };
        if let Some(client_info) = self.app_server_client_info {
            session
                .services
                .mcp_runtime
                .set_elicitations_auto_deny(client_info.mcp_elicitations_auto_deny);
        }
        if let Some(turn_trigger) = turn_trigger {
            turn_context
                .turn_metadata_state
                .set_turn_trigger(turn_trigger);
        }
        if emit_thread_settings_applied {
            thread_settings::emit_applied(session, submission_id, settings_snapshot).await;
        }
        if let Some(parent_turn_id) = parent_turn_id {
            turn_context
                .turn_metadata_state
                .set_parent_turn_id(parent_turn_id);
        }
        if let Some(root_turn_id) = root_turn_id {
            turn_context
                .turn_metadata_state
                .set_root_turn_id(root_turn_id);
        }
        Ok(Some(turn_context))
    }

    /// Applies only persistent settings after steering succeeds. The active
    /// turn keeps its existing context; subsequent turns see the update.
    pub(super) async fn apply_steered(
        self,
        session: &Session,
        submission_id: String,
    ) -> CodexResult<()> {
        if self.thread_settings_update.is_none() && self.app_server_client_info.is_none() {
            return Ok(());
        }
        let emit_thread_settings_applied = self.thread_settings_update.is_some();
        let mut updates = self.thread_settings_update.unwrap_or_default();
        if let Some(client_info) = &self.app_server_client_info {
            updates.app_server_client_name = client_info.name.clone();
            updates.app_server_client_version = client_info.version.clone();
        }
        if emit_thread_settings_applied {
            thread_settings::apply_update(session, submission_id, updates)
                .await
                .map_err(|error| CodexErr::InvalidRequest(error.to_string()))?;
        } else {
            session
                .update_settings(updates)
                .await
                .map_err(|error| CodexErr::InvalidRequest(error.to_string()))?;
        }
        if let Some(client_info) = self.app_server_client_info {
            session
                .services
                .mcp_runtime
                .set_elicitations_auto_deny(client_info.mcp_elicitations_auto_deny);
        }
        Ok(())
    }
}

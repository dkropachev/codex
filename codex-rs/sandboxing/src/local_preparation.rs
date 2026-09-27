use std::process::Command;

use codex_protocol::config_types::WindowsSandboxLevel;
use codex_utils_absolute_path::AbsolutePathBuf;

use crate::LocalProcessCommand;
use crate::LocalSandboxLaunchPolicy;
use crate::LocalSandboxSelection;
use crate::SandboxDirectSpawnRuntime;
use crate::SandboxManager;
use crate::SandboxType;
use crate::SandboxablePreference;
use crate::prepare_unrestricted_command;

/// Trusted host-local paths and platform settings used to prepare a sandboxed command.
pub struct LocalSandboxRuntime<'a> {
    pub direct_spawn: SandboxDirectSpawnRuntime<'a>,
    pub linux_sandbox_executable: Option<&'a AbsolutePathBuf>,
    pub use_legacy_landlock: bool,
    pub windows_sandbox_level: WindowsSandboxLevel,
    pub windows_sandbox_private_desktop: bool,
}

/// Inputs for preparing a selected local sandbox command without spawning it.
pub struct LocalSandboxPreparationRequest<'a> {
    pub command: LocalProcessCommand,
    pub selection: LocalSandboxSelection,
    pub policy: LocalSandboxLaunchPolicy,
    pub sandbox_policy_cwd: &'a AbsolutePathBuf,
    pub workspace_roots: &'a [AbsolutePathBuf],
    pub runtime: LocalSandboxRuntime<'a>,
}

/// An unspawned command and the sandbox selected for its inner process.
pub struct PreparedLocalSandboxCommand {
    command: Command,
    sandbox: SandboxType,
}

impl PreparedLocalSandboxCommand {
    pub fn into_command(self) -> Command {
        self.command
    }

    pub fn sandbox(&self) -> SandboxType {
        self.sandbox
    }
}

/// Why the selected local sandbox could not be prepared safely.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalSandboxUnavailableReason {
    SelectionUnavailable,
    MissingLinuxSandboxExecutable,
    MissingWindowsSandboxWrapper,
    UnrepresentableInput(&'static str),
    PlatformPreparation,
}

/// Result of preparation when a caller may later offer an exact trust fallback.
pub enum LocalSandboxPreparation {
    Prepared(PreparedLocalSandboxCommand),
    Unavailable(LocalSandboxUnavailableReason),
}

/// Prepares a host-local command without spawning it or weakening its selection.
///
/// Platform-sandbox selections fail closed until the platform preparation
/// stage is applied. An unrestricted command is prepared only when both the
/// selection and its originating policy permit one.
pub fn prepare_local_sandbox_command(
    request: LocalSandboxPreparationRequest<'_>,
) -> anyhow::Result<LocalSandboxPreparation> {
    let LocalSandboxPreparationRequest {
        command,
        selection,
        policy,
        sandbox_policy_cwd: _,
        workspace_roots: _,
        runtime: _,
    } = request;
    let LocalSandboxSelection::Selected {
        sandbox,
        permissions,
    } = selection
    else {
        return Ok(LocalSandboxPreparation::Unavailable(
            LocalSandboxUnavailableReason::SelectionUnavailable,
        ));
    };
    if sandbox != SandboxType::None {
        return Ok(LocalSandboxPreparation::Unavailable(
            LocalSandboxUnavailableReason::PlatformPreparation,
        ));
    }
    let preference = match policy {
        LocalSandboxLaunchPolicy::Required => SandboxablePreference::Require,
        LocalSandboxLaunchPolicy::FollowPermissionProfile => SandboxablePreference::Auto,
    };
    if SandboxManager::new().should_sandbox(
        &permissions,
        preference,
        /*has_managed_network_requirements*/ false,
    ) {
        return Ok(LocalSandboxPreparation::Unavailable(
            LocalSandboxUnavailableReason::SelectionUnavailable,
        ));
    }
    Ok(LocalSandboxPreparation::Prepared(
        PreparedLocalSandboxCommand {
            command: prepare_unrestricted_command(command),
            sandbox,
        },
    ))
}

#[cfg(test)]
#[path = "local_preparation_tests.rs"]
mod tests;

use std::process::Command;

use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::permissions::FileSystemPath;
use codex_utils_absolute_path::AbsolutePathBuf;

use crate::LocalProcessCommand;
use crate::LocalSandboxLaunchPolicy;
use crate::LocalSandboxSelection;
use crate::SandboxDirectSpawnRuntime;
use crate::SandboxManager;
use crate::SandboxType;
use crate::SandboxablePreference;
use crate::get_platform_sandbox;
use crate::prepare_sandbox_command;
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
        sandbox_policy_cwd,
        workspace_roots,
        runtime,
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
        if get_platform_sandbox(runtime.windows_sandbox_level != WindowsSandboxLevel::Disabled)
            != Some(sandbox)
        {
            return Ok(LocalSandboxPreparation::Unavailable(
                LocalSandboxUnavailableReason::PlatformPreparation,
            ));
        }
        if sandbox == SandboxType::LinuxSeccomp && runtime.linux_sandbox_executable.is_none() {
            return Ok(LocalSandboxPreparation::Unavailable(
                LocalSandboxUnavailableReason::MissingLinuxSandboxExecutable,
            ));
        }
        if sandbox == SandboxType::WindowsRestrictedToken
            && runtime
                .direct_spawn
                .windows_sandbox_wrapper_executable
                .is_none()
        {
            return Ok(LocalSandboxPreparation::Unavailable(
                LocalSandboxUnavailableReason::MissingWindowsSandboxWrapper,
            ));
        }
        if let Some(input) = unrepresentable_path(
            &permissions,
            sandbox_policy_cwd,
            workspace_roots,
            &runtime,
            &command.cwd,
            sandbox,
        ) {
            return Ok(LocalSandboxPreparation::Unavailable(
                LocalSandboxUnavailableReason::UnrepresentableInput(input),
            ));
        }
        if let Err(error) = prepare_sandbox_command(command) {
            return Ok(LocalSandboxPreparation::Unavailable(
                LocalSandboxUnavailableReason::UnrepresentableInput(error.input()),
            ));
        }
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

fn unrepresentable_path(
    permissions: &codex_protocol::models::PermissionProfile,
    sandbox_policy_cwd: &AbsolutePathBuf,
    workspace_roots: &[AbsolutePathBuf],
    runtime: &LocalSandboxRuntime<'_>,
    command_cwd: &AbsolutePathBuf,
    sandbox: SandboxType,
) -> Option<&'static str> {
    let valid = |path: &AbsolutePathBuf| {
        path.as_path()
            .to_str()
            .is_some_and(|path| !path.contains('\0'))
    };
    for (label, path) in [
        ("command cwd", command_cwd),
        ("sandbox policy cwd", sandbox_policy_cwd),
    ] {
        if !valid(path) {
            return Some(label);
        }
    }
    let file_system_policy = permissions.file_system_sandbox_policy();
    for entry in &file_system_policy.entries {
        match &entry.path {
            FileSystemPath::Path { path }
                if path
                    .to_abs_path()
                    .ok()
                    .as_ref()
                    .is_none_or(|path| !valid(path)) =>
            {
                return Some("permission path");
            }
            FileSystemPath::GlobPattern { pattern } if pattern.contains('\0') => {
                return Some("permission glob");
            }
            FileSystemPath::Path { .. }
            | FileSystemPath::GlobPattern { .. }
            | FileSystemPath::Special { .. } => {}
        }
    }
    for (label, paths) in [
        (
            "resolved readable root",
            file_system_policy.get_readable_roots_with_cwd(sandbox_policy_cwd.as_path()),
        ),
        (
            "resolved unreadable root",
            file_system_policy.get_unreadable_roots_with_cwd(sandbox_policy_cwd.as_path()),
        ),
    ] {
        if paths.iter().any(|path| !valid(path)) {
            return Some(label);
        }
    }
    for writable in file_system_policy.get_writable_roots_with_cwd(sandbox_policy_cwd.as_path()) {
        if !valid(&writable.root) {
            return Some("resolved writable root");
        }
        if writable.read_only_subpaths.iter().any(|path| !valid(path)) {
            return Some("resolved writable carveout");
        }
    }
    if sandbox == SandboxType::LinuxSeccomp
        && runtime
            .linux_sandbox_executable
            .is_some_and(|path| !valid(path))
    {
        return Some("Linux sandbox executable");
    }
    if sandbox == SandboxType::WindowsRestrictedToken {
        if !valid(runtime.direct_spawn.codex_home) {
            return Some("Codex home");
        }
        if runtime
            .direct_spawn
            .windows_sandbox_wrapper_executable
            .is_some_and(|path| !valid(path))
        {
            return Some("Windows sandbox wrapper");
        }
        if workspace_roots.iter().any(|path| !valid(path)) {
            return Some("workspace root");
        }
    }
    None
}

#[cfg(test)]
#[path = "local_preparation_tests.rs"]
mod tests;

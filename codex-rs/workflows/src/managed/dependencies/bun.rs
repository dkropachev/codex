#![allow(dead_code, reason = "used by managed dependency installation")]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use anyhow::Context;
use anyhow::bail;
use codex_protocol::models::PermissionProfile;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::FileSystemSpecialPath;
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_sandboxing::LocalProcessCommand;
use codex_sandboxing::LocalSandboxLaunchPolicy;
use codex_sandboxing::LocalSandboxPreparation;
use codex_sandboxing::LocalSandboxPreparationRequest;
use codex_sandboxing::LocalSandboxRuntime;
use codex_sandboxing::LocalSandboxUnavailableReason;
use codex_sandboxing::SandboxDirectSpawnRuntime;
use codex_sandboxing::SandboxType;
use codex_sandboxing::prepare_local_sandbox_command;
use codex_sandboxing::select_local_sandbox;
use codex_utils_absolute_path::AbsolutePathBuf;

use super::ValidatedDependencySources;

mod paths;

use paths::absolute_from_path;
use paths::paths_overlap;

const PUBLIC_REGISTRY: &str = "https://registry.npmjs.org/";
const TRUSTED_BUNFIG: &str =
    "env = false\ntelemetry = false\n\n[install]\nregistry = \"https://registry.npmjs.org/\"\n";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ManagedBunOperation {
    Install,
    InspectBinaryLockfile,
}

/// The committed lockfile protected from mutation during a managed install.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::managed) enum ManagedBunInstallLockfile {
    Text,
    Binary,
}

impl ManagedBunInstallLockfile {
    fn file_name(self) -> &'static str {
        match self {
            Self::Text => "bun.lock",
            Self::Binary => "bun.lockb",
        }
    }
}

/// Trusted persistent and operation-private paths used by managed Bun commands.
#[derive(Debug)]
pub(in crate::managed) struct ManagedBunEnvironment {
    pub(in crate::managed) cache_dir: AbsolutePathBuf,
    pub(in crate::managed) scratch_dir: AbsolutePathBuf,
    pub(in crate::managed) temp_dir: AbsolutePathBuf,
    pub(in crate::managed) home_dir: AbsolutePathBuf,
    pub(in crate::managed) xdg_config_dir: AbsolutePathBuf,
    pub(in crate::managed) xdg_cache_dir: AbsolutePathBuf,
    pub(in crate::managed) xdg_data_dir: AbsolutePathBuf,
    pub(in crate::managed) xdg_state_dir: AbsolutePathBuf,
    pub(in crate::managed) app_data_dir: AbsolutePathBuf,
    pub(in crate::managed) local_app_data_dir: AbsolutePathBuf,
    pub(in crate::managed) bunfig: AbsolutePathBuf,
    pub(in crate::managed) npmrc: AbsolutePathBuf,
    operation: Arc<tempfile::TempDir>,
}

/// An unspawned Bun command together with the exact sandbox permissions it requires.
#[derive(Debug)]
pub(in crate::managed) struct ManagedBunCommandPlan {
    program: AbsolutePathBuf,
    args: Vec<OsString>,
    cwd: AbsolutePathBuf,
    env: BTreeMap<OsString, OsString>,
    permissions: PermissionProfile,
    operation: Arc<tempfile::TempDir>,
}

/// Mandatory-sandbox preparation for a managed Bun command.
pub(in crate::managed) enum ManagedBunSandboxPreparation {
    Prepared(PreparedManagedBunCommand),
    Unavailable(LocalSandboxUnavailableReason),
}

/// An unspawned sandbox command that retains its operation-private environment.
pub(in crate::managed) struct PreparedManagedBunCommand {
    command: Command,
    sandbox: SandboxType,
    _operation: Arc<tempfile::TempDir>,
}

impl PreparedManagedBunCommand {
    pub(in crate::managed) fn run(
        self,
        deadline: crate::runner::CommandDeadline,
        limits: crate::runner::CommandOutputLimits,
        cancelled: Option<&AtomicBool>,
    ) -> anyhow::Result<crate::runner::BoundedCommandOutput> {
        let Self {
            command,
            sandbox: _,
            _operation,
        } = self;
        let output = crate::runner::run_bounded_command_until_with_limits(
            command, deadline, limits, cancelled,
        );
        drop(_operation);
        output
    }
}

impl ManagedBunCommandPlan {
    /// Selects and prepares mandatory local sandboxing without weakening an unavailable result.
    pub(in crate::managed) fn prepare(
        self,
        runtime: &LocalSandboxRuntime<'_>,
    ) -> anyhow::Result<ManagedBunSandboxPreparation> {
        let workspace_roots = [self.cwd.clone()];
        let selection = select_local_sandbox(
            &self.permissions,
            &workspace_roots,
            LocalSandboxLaunchPolicy::Required,
            runtime.windows_sandbox_level,
        );
        let preparation = prepare_local_sandbox_command(LocalSandboxPreparationRequest {
            command: LocalProcessCommand {
                program: self.program.as_path().as_os_str().to_os_string(),
                args: self.args,
                cwd: self.cwd.clone(),
                env: self.env.into_iter().collect(),
            },
            selection,
            policy: LocalSandboxLaunchPolicy::Required,
            sandbox_policy_cwd: &self.cwd,
            workspace_roots: &workspace_roots,
            runtime: LocalSandboxRuntime {
                direct_spawn: SandboxDirectSpawnRuntime {
                    codex_home: runtime.direct_spawn.codex_home,
                    windows_sandbox_wrapper_executable: runtime
                        .direct_spawn
                        .windows_sandbox_wrapper_executable,
                },
                linux_sandbox_executable: runtime.linux_sandbox_executable,
                use_legacy_landlock: runtime.use_legacy_landlock,
                windows_sandbox_level: runtime.windows_sandbox_level,
                windows_sandbox_private_desktop: runtime.windows_sandbox_private_desktop,
            },
        })?;
        Ok(match preparation {
            LocalSandboxPreparation::Prepared(prepared) => {
                let sandbox = prepared.sandbox();
                ManagedBunSandboxPreparation::Prepared(PreparedManagedBunCommand {
                    command: prepared.into_command(),
                    sandbox,
                    _operation: self.operation,
                })
            }
            LocalSandboxPreparation::Unavailable(reason) => {
                ManagedBunSandboxPreparation::Unavailable(reason)
            }
        })
    }
}

/// Creates a persistent cache and a fresh credential-free environment for one Bun operation.
pub(in crate::managed) fn materialize_bun_environment(
    management_root: &AbsolutePathBuf,
) -> anyhow::Result<ManagedBunEnvironment> {
    let bun_root = management_root.join("bun");
    let cache_dir = bun_root.join("cache");
    let operations_dir = bun_root.join("operations");
    super::install::ensure_regular_directory_tree(management_root, &cache_dir)
        .context("failed to create managed Bun cache")?;
    super::install::ensure_regular_directory_tree(management_root, &operations_dir)
        .context("failed to create managed Bun operation root")?;

    let operation = tempfile::Builder::new()
        .prefix("operation-")
        .tempdir_in(operations_dir.as_path())
        .context("failed to create private managed Bun operation directory")?;
    let operation_root = AbsolutePathBuf::from_absolute_path_checked(operation.path())
        .context("managed Bun operation directory was not absolute")?;
    let scratch_dir = operation_root.join("scratch");
    let temp_dir = operation_root.join("temp");
    let home_dir = operation_root.join("home");
    let xdg_config_dir = home_dir.join("xdg-config");
    let xdg_cache_dir = home_dir.join("xdg-cache");
    let xdg_data_dir = home_dir.join("xdg-data");
    let xdg_state_dir = home_dir.join("xdg-state");
    let app_data_dir = home_dir.join("app-data");
    let local_app_data_dir = home_dir.join("local-app-data");
    for path in [
        &scratch_dir,
        &temp_dir,
        &home_dir,
        &xdg_config_dir,
        &xdg_cache_dir,
        &xdg_data_dir,
        &xdg_state_dir,
        &app_data_dir,
        &local_app_data_dir,
    ] {
        fs::create_dir_all(path.as_path()).with_context(|| {
            format!(
                "failed to create managed Bun directory {}",
                path.as_path().display()
            )
        })?;
    }

    let bunfig = operation_root.join("bunfig.toml");
    let npmrc = operation_root.join("npmrc");
    fs::write(bunfig.as_path(), TRUSTED_BUNFIG).with_context(|| {
        format!(
            "failed to write trusted Bun configuration {}",
            bunfig.as_path().display()
        )
    })?;
    fs::write(npmrc.as_path(), []).with_context(|| {
        format!(
            "failed to write empty npm configuration {}",
            npmrc.as_path().display()
        )
    })?;

    Ok(ManagedBunEnvironment {
        cache_dir,
        scratch_dir,
        temp_dir,
        home_dir,
        xdg_config_dir,
        xdg_cache_dir,
        xdg_data_dir,
        xdg_state_dir,
        app_data_dir,
        local_app_data_dir,
        bunfig,
        npmrc,
        operation: Arc::new(operation),
    })
}

/// Rejects candidate-local npm credentials and Bun-loaded environment files.
pub(in crate::managed) fn reject_untrusted_candidate_bun_configuration(
    candidate: &AbsolutePathBuf,
) -> anyhow::Result<()> {
    for entry in fs::read_dir(candidate.as_path()).with_context(|| {
        format!(
            "failed to inspect managed workflow candidate {}",
            candidate.as_path().display()
        )
    })? {
        let entry = entry.with_context(|| {
            format!(
                "failed to inspect an entry in managed workflow candidate {}",
                candidate.as_path().display()
            )
        })?;
        let Some(name) = entry.file_name().to_str().map(str::to_ascii_lowercase) else {
            continue;
        };
        if name == ".npmrc" || name == ".env" || name.starts_with(".env.") {
            bail!(
                "managed workflow candidate may not contain repository-controlled configuration `{}`",
                entry.file_name().to_string_lossy()
            );
        }
    }
    Ok(())
}

/// Plans a frozen, script-free dependency install into the managed candidate.
pub(in crate::managed) fn managed_bun_install_command_plan(
    bun_executable: &AbsolutePathBuf,
    candidate: &AbsolutePathBuf,
    lockfile: ManagedBunInstallLockfile,
    sources: &ValidatedDependencySources,
    environment: &ManagedBunEnvironment,
) -> anyhow::Result<ManagedBunCommandPlan> {
    reject_untrusted_candidate_bun_configuration(candidate)?;
    if paths_overlap(candidate, &environment.cache_dir)?
        || paths_overlap(
            candidate,
            &absolute_from_path(environment.operation.path())?,
        )?
    {
        bail!("managed Bun environment and workflow candidate must not overlap");
    }
    let bun_executable = validate_bun_executable(
        bun_executable,
        &[
            candidate,
            &environment.cache_dir,
            &environment.temp_dir,
            &environment.home_dir,
        ],
    )?;
    let mut read_only_paths = vec![
        candidate.join("package.json"),
        candidate.join(lockfile.file_name()),
        candidate.join(".git"),
    ];
    read_only_paths.extend(
        sources
            .local_packages
            .iter()
            .map(|package| candidate.join(package).join("package.json")),
    );
    Ok(command_plan(
        &bun_executable,
        candidate,
        environment,
        ManagedBunOperation::Install,
        NetworkSandboxPolicy::Enabled,
        &read_only_paths,
    ))
}

/// Plans sandbox-only inspection of a binary lockfile in private scratch space.
///
/// The caller must first copy the candidate's `package.json` and `bun.lockb` into the returned
/// environment's scratch directory. Copying and executing the plan are intentionally separate.
pub(in crate::managed) fn managed_bun_binary_inspection_command_plan(
    bun_executable: &AbsolutePathBuf,
    environment: &ManagedBunEnvironment,
) -> anyhow::Result<ManagedBunCommandPlan> {
    let bun_executable = validate_bun_executable(
        bun_executable,
        &[
            &environment.scratch_dir,
            &environment.cache_dir,
            &environment.temp_dir,
            &environment.home_dir,
        ],
    )?;
    Ok(command_plan(
        &bun_executable,
        &environment.scratch_dir,
        environment,
        ManagedBunOperation::InspectBinaryLockfile,
        NetworkSandboxPolicy::Restricted,
        &[environment.scratch_dir.join("package.json")],
    ))
}

fn validate_bun_executable(
    bun_executable: &AbsolutePathBuf,
    writable_roots: &[&AbsolutePathBuf],
) -> anyhow::Result<AbsolutePathBuf> {
    let bun = fs::canonicalize(bun_executable.as_path()).with_context(|| {
        format!(
            "failed to resolve managed Bun executable {}",
            bun_executable.as_path().display()
        )
    })?;
    if !bun.is_file() {
        bail!("managed Bun executable must be a regular file");
    }
    for writable_root in writable_roots {
        let writable_root = fs::canonicalize(writable_root.as_path()).with_context(|| {
            format!(
                "failed to resolve managed writable root {}",
                writable_root.as_path().display()
            )
        })?;
        if bun.starts_with(writable_root) {
            bail!("managed writable root may not provide the Bun executable");
        }
    }
    AbsolutePathBuf::from_absolute_path_checked(bun)
        .context("resolved managed Bun executable was not absolute")
}

fn command_plan(
    bun_executable: &AbsolutePathBuf,
    target: &AbsolutePathBuf,
    environment: &ManagedBunEnvironment,
    operation: ManagedBunOperation,
    network: NetworkSandboxPolicy,
    read_only_paths: &[AbsolutePathBuf],
) -> ManagedBunCommandPlan {
    let mut config_argument = OsString::from("--config=");
    config_argument.push(environment.bunfig.as_path());
    let mut args = vec!["--no-env-file".into(), config_argument, "install".into()];
    if operation == ManagedBunOperation::InspectBinaryLockfile {
        args.extend(["--save-text-lockfile".into(), "--lockfile-only".into()]);
    }
    args.extend([
        "--frozen-lockfile".into(),
        "--ignore-scripts".into(),
        "--backend=copyfile".into(),
        format!("--registry={PUBLIC_REGISTRY}").into(),
        "--cache-dir".into(),
        environment.cache_dir.as_path().as_os_str().to_os_string(),
    ]);
    let writable_target = match operation {
        ManagedBunOperation::Install => target.join("node_modules"),
        ManagedBunOperation::InspectBinaryLockfile => target.clone(),
    };
    let mut entries = vec![FileSystemSandboxEntry::new(
        FileSystemPath::Special {
            value: FileSystemSpecialPath::Root,
        },
        FileSystemAccessMode::Read,
    )];
    entries.extend(
        [
            &writable_target,
            &environment.cache_dir,
            &environment.temp_dir,
            &environment.home_dir,
        ]
        .into_iter()
        .cloned()
        .map(|path| FileSystemSandboxEntry::new(path.into(), FileSystemAccessMode::Write)),
    );
    entries.extend(
        read_only_paths
            .iter()
            .cloned()
            .chain([environment.bunfig.clone(), environment.npmrc.clone()])
            .map(|path| FileSystemSandboxEntry::new(path.into(), FileSystemAccessMode::Read)),
    );
    let permissions = PermissionProfile::from_runtime_permissions(
        &FileSystemSandboxPolicy::restricted(entries),
        network,
    );

    ManagedBunCommandPlan {
        program: bun_executable.clone(),
        args,
        cwd: target.clone(),
        env: command_environment(environment),
        permissions,
        operation: Arc::clone(&environment.operation),
    }
}

fn command_environment(environment: &ManagedBunEnvironment) -> BTreeMap<OsString, OsString> {
    let mut env = BTreeMap::new();
    for (name, value) in [
        ("HOME", &environment.home_dir),
        ("USERPROFILE", &environment.home_dir),
        ("XDG_CONFIG_HOME", &environment.xdg_config_dir),
        ("XDG_CACHE_HOME", &environment.xdg_cache_dir),
        ("XDG_DATA_HOME", &environment.xdg_data_dir),
        ("XDG_STATE_HOME", &environment.xdg_state_dir),
        ("APPDATA", &environment.app_data_dir),
        ("LOCALAPPDATA", &environment.local_app_data_dir),
        ("TMPDIR", &environment.temp_dir),
        ("TEMP", &environment.temp_dir),
        ("TMP", &environment.temp_dir),
        ("BUN_INSTALL_CACHE_DIR", &environment.cache_dir),
        ("BUN_CONFIG_FILE", &environment.bunfig),
        ("NPM_CONFIG_USERCONFIG", &environment.npmrc),
        ("NPM_CONFIG_GLOBALCONFIG", &environment.npmrc),
    ] {
        env.insert(name.into(), value.as_path().as_os_str().to_os_string());
    }
    env.insert("NPM_CONFIG_REGISTRY".into(), PUBLIC_REGISTRY.into());
    env
}

#[cfg(test)]
#[path = "bun_tests.rs"]
mod tests;

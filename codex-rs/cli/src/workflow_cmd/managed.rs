use std::fs;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use anyhow::Context;
use anyhow::bail;
use codex_core::config::Config;
use codex_core::windows_sandbox::WindowsSandboxLevelExt;
use codex_protocol::config_types::WindowsSandboxLevel;
use codex_sandboxing::LocalSandboxRuntime;
use codex_sandboxing::SandboxDirectSpawnRuntime;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_workflows::ManagedWorkflowDependencyRuntime;
use codex_workflows::ManagedWorkflowInstallRequest;
use codex_workflows::ManagedWorkflowRecord;
use codex_workflows::ManagedWorkflowService;
use codex_workflows::ManagedWorkflowUpdate;
use codex_workflows::ManagedWorkflowUpdateRequest;
use codex_workflows::WorkflowCommand;
use codex_workflows::WorkflowUpdatePolicy;
use serde_json::Value;
use serde_json::json;

struct DependencyRuntimePaths {
    bun: Option<AbsolutePathBuf>,
    linux_sandbox: Option<AbsolutePathBuf>,
    windows_wrapper: Option<AbsolutePathBuf>,
}

impl DependencyRuntimePaths {
    fn from_config(config: &Config) -> anyhow::Result<Self> {
        let bun = which::which("bun")
            .ok()
            .map(AbsolutePathBuf::from_absolute_path_checked)
            .transpose()?;
        let linux_sandbox = config
            .codex_linux_sandbox_exe
            .as_ref()
            .map(AbsolutePathBuf::from_absolute_path_checked)
            .transpose()?;
        let windows_wrapper = config
            .codex_self_exe
            .as_ref()
            .map(AbsolutePathBuf::from_absolute_path_checked)
            .transpose()?;
        Ok(Self {
            bun,
            linux_sandbox,
            windows_wrapper,
        })
    }

    fn runtime<'a>(&'a self, config: &'a Config) -> Option<ManagedWorkflowDependencyRuntime<'a>> {
        let bun_executable = self.bun.as_ref()?;
        Some(ManagedWorkflowDependencyRuntime {
            bun_executable,
            sandbox: LocalSandboxRuntime {
                direct_spawn: SandboxDirectSpawnRuntime {
                    codex_home: &config.codex_home,
                    windows_sandbox_wrapper_executable: self.windows_wrapper.as_ref(),
                },
                linux_sandbox_executable: self.linux_sandbox.as_ref(),
                use_legacy_landlock: config.features.use_legacy_landlock(),
                windows_sandbox_level: WindowsSandboxLevel::from_config(config),
                windows_sandbox_private_desktop: false,
            },
        })
    }
}

pub(super) fn run(args: &[String], config: &Config) -> anyhow::Result<()> {
    let service =
        ManagedWorkflowService::new(&config.codex_home, &config.codex_home.join("workflows"))?;
    let cancelled = AtomicBool::new(false);
    match args.first().map(String::as_str) {
        Some("install") => {
            let source = args.get(1).context("install requires a Git source")?;
            let policy = match args.get(2..).unwrap_or_default() {
                [] => WorkflowUpdatePolicy::Prompt,
                [flag, value] if flag == "--policy" => parse_policy(value)?,
                _ => bail!(
                    "usage: codex workflow install <source> [--policy prompt|automatic|manual]"
                ),
            };
            let paths = DependencyRuntimePaths::from_config(config)?;
            let installed = service.install_with_policy(
                ManagedWorkflowInstallRequest {
                    source,
                    dependency_runtime: paths.runtime(config),
                    cancelled: &cancelled,
                },
                policy,
            )?;
            println!(
                "Installed {} at {}",
                installed.id, installed.release.advertised_object_id
            );
            if installed.cleanup_pending {
                eprintln!("Managed workflow cleanup remains pending; it will finish at startup.");
            }
        }
        Some("check-updates") => {
            if args.len() > 2 {
                bail!("usage: codex workflow check-updates [id]");
            }
            let records = service.list_installed()?;
            let ids: Vec<_> = match args.get(1) {
                Some(id) => vec![record(&records, id)?.id.as_str()],
                None => records.iter().map(|record| record.id.as_str()).collect(),
            };
            for id in ids {
                let check = service.check_update(id, &cancelled)?;
                match check.update {
                    ManagedWorkflowUpdate::Current => println!("{id}: current"),
                    ManagedWorkflowUpdate::Available { release, dismissed } => {
                        let label = release.tag.as_deref().unwrap_or(&release.commit);
                        if dismissed {
                            println!("{id}: {label} available (dismissed)");
                        } else {
                            println!("{id}: {label} available");
                        }
                    }
                    ManagedWorkflowUpdate::Error(error) => println!("{id}: error: {error}"),
                }
            }
        }
        Some("update") => {
            if args.len() != 2 {
                bail!("usage: codex workflow update <id>|--all");
            }
            let records = service.list_installed()?;
            let all = args[1] == "--all";
            let ids: Vec<_> = if all {
                records.iter().map(|record| record.id.as_str()).collect()
            } else {
                vec![record(&records, &args[1])?.id.as_str()]
            };
            let paths = DependencyRuntimePaths::from_config(config)?;
            let mut failures = 0;
            for id in ids {
                let result = (|| -> anyhow::Result<()> {
                    let check = service.check_update(id, &cancelled)?;
                    match check.update {
                        ManagedWorkflowUpdate::Available { release, .. } => {
                            let installed = service.update(ManagedWorkflowUpdateRequest {
                                id,
                                expected_installed: &check.workflow.installed,
                                expected_available: &release,
                                dependency_runtime: paths.runtime(config),
                                cancelled: &cancelled,
                            })?;
                            println!("Updated {id} to {}", installed.release.advertised_object_id);
                        }
                        ManagedWorkflowUpdate::Current if all => {}
                        ManagedWorkflowUpdate::Current => bail!("{id} has no eligible update"),
                        ManagedWorkflowUpdate::Error(error) => bail!("{id}: {error}"),
                    }
                    Ok(())
                })();
                if let Err(error) = result {
                    if !all {
                        return Err(error);
                    }
                    eprintln!("{id}: {error:#}");
                    failures += 1;
                }
            }
            if failures > 0 {
                bail!("{failures} managed workflow update(s) failed");
            }
        }
        Some("set-policy") => {
            if args.len() != 3 {
                bail!("usage: codex workflow set-policy <id> <prompt|automatic|manual>");
            }
            let policy = parse_policy(&args[2])?;
            let records = service.list_installed()?;
            let current = record(&records, &args[1])?;
            service.set_policy(&current.id, &current.installed, policy, &cancelled)?;
            println!("{} policy set to {}", current.id, policy_name(policy));
        }
        Some("dismiss") => {
            if args.len() != 2 {
                bail!("usage: codex workflow dismiss <id>");
            }
            let check = service.check_update(&args[1], &cancelled)?;
            let release = match check.update {
                ManagedWorkflowUpdate::Available { release, .. } => release,
                ManagedWorkflowUpdate::Current => {
                    bail!("{} has no eligible update to dismiss", args[1]);
                }
                ManagedWorkflowUpdate::Error(error) => bail!("{}: {error}", args[1]),
            };
            service.dismiss_release(&args[1], &check.workflow.installed, &release, &cancelled)?;
            println!(
                "Dismissed {} for {}",
                release.tag.as_deref().unwrap_or(&release.commit),
                args[1]
            );
        }
        Some("uninstall") => {
            if args.len() != 2 {
                bail!("usage: codex workflow uninstall <id>");
            }
            let records = service.list_installed()?;
            let current = record(&records, &args[1])?;
            let removed = service.uninstall(&current.id, &current.installed, &cancelled)?;
            println!("Uninstalled {}", removed.id);
            if removed.cleanup_pending {
                eprintln!("Managed workflow cleanup remains pending; it will finish at startup.");
            }
        }
        Some(command) => bail!("unknown managed workflow command '{command}'"),
        None => bail!("missing managed workflow command"),
    }
    Ok(())
}

fn parse_policy(value: &str) -> anyhow::Result<WorkflowUpdatePolicy> {
    match value {
        "prompt" => Ok(WorkflowUpdatePolicy::Prompt),
        "automatic" => Ok(WorkflowUpdatePolicy::Automatic),
        "manual" => Ok(WorkflowUpdatePolicy::Manual),
        _ => bail!("unknown workflow update policy '{value}'"),
    }
}

fn policy_name(policy: WorkflowUpdatePolicy) -> &'static str {
    match policy {
        WorkflowUpdatePolicy::Prompt => "prompt",
        WorkflowUpdatePolicy::Automatic => "automatic",
        WorkflowUpdatePolicy::Manual => "manual",
    }
}

pub(super) fn records(config: &Config) -> anyhow::Result<Vec<ManagedWorkflowRecord>> {
    let management = config.codex_home.join(".workflow-management");
    let has_entries = |root: &AbsolutePathBuf| -> anyhow::Result<bool> {
        Ok(root.as_path().is_dir() && fs::read_dir(root.as_path())?.next().is_some())
    };
    if !has_entries(&management.join("receipts"))? && !has_entries(&management.join("journals"))? {
        return Ok(Vec::new());
    }
    ManagedWorkflowService::new(&config.codex_home, &config.codex_home.join("workflows"))?
        .list_installed()
}

pub(super) fn record_for_command<'a>(
    config: &Config,
    command: &WorkflowCommand,
    records: &'a [ManagedWorkflowRecord],
) -> Option<&'a ManagedWorkflowRecord> {
    let global_root = config.codex_home.join("workflows");
    records
        .iter()
        .find(|record| command.workflow_dir == global_root.join(&record.id).as_path())
}

pub(super) fn reject_managed_edit(config: &Config, id: &str, path: &Path) -> anyhow::Result<()> {
    let global_root = config.codex_home.join("workflows");
    if path.starts_with(global_root.as_path())
        && records(config)?
            .iter()
            .any(|record| path == global_root.join(&record.id).as_path())
    {
        bail!("{id} is a managed workflow; update or uninstall it instead");
    }
    Ok(())
}

pub(super) fn release_summary(record: &ManagedWorkflowRecord) -> String {
    let label = record
        .installed
        .tag
        .as_deref()
        .or(record.installed.version.as_deref())
        .unwrap_or(&record.installed.commit);
    format!("managed {label}; policy {}", policy_name(record.policy))
}

pub(super) fn release_json(record: &ManagedWorkflowRecord) -> Value {
    json!({
        "source": record.source,
        "installed": {
            "tag": record.installed.tag,
            "version": record.installed.version,
            "commit": record.installed.commit,
        },
        "policy": policy_name(record.policy),
        "dismissedRelease": record.dismissed_release.as_ref().map(|release| json!({
            "tag": release.tag,
            "version": release.version,
            "commit": release.commit,
        })),
    })
}

fn record<'a>(
    records: &'a [ManagedWorkflowRecord],
    id: &str,
) -> anyhow::Result<&'a ManagedWorkflowRecord> {
    records
        .iter()
        .find(|record| record.id == id)
        .with_context(|| format!("managed workflow '{id}' is not installed"))
}

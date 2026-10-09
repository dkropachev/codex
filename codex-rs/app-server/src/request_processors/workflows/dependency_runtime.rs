use codex_core::config::Config;
use codex_core::windows_sandbox::WindowsSandboxLevelExt;
use codex_protocol::config_types::WindowsSandboxLevel;
use codex_sandboxing::LocalSandboxRuntime;
use codex_sandboxing::SandboxDirectSpawnRuntime;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_workflows::ManagedWorkflowDependencyRuntime;

pub(super) struct DependencyRuntimePaths {
    bun: Option<AbsolutePathBuf>,
    linux_sandbox: Option<AbsolutePathBuf>,
    windows_wrapper: Option<AbsolutePathBuf>,
}

impl DependencyRuntimePaths {
    pub(super) fn from_config(config: &Config) -> anyhow::Result<Self> {
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

    pub(super) fn runtime<'a>(
        &'a self,
        config: &'a Config,
    ) -> Option<ManagedWorkflowDependencyRuntime<'a>> {
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

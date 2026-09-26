#[cfg(any(not(debug_assertions), test))]
use codex_install_context::InstallContext;
#[cfg(any(not(debug_assertions), test))]
use codex_install_context::StandalonePlatform;

pub(crate) const FORK_INSTALLER_URL: &str =
    "https://github.com/dkropachev/codex/releases/latest/download/install.sh";

/// Update action the CLI should perform after the TUI exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateAction {
    /// Update through the installer published with the latest fork release.
    StandaloneUnix,
}

impl UpdateAction {
    #[cfg(any(not(debug_assertions), test))]
    pub(crate) fn from_install_context(context: &InstallContext) -> Option<Self> {
        match context.managed_fork_standalone_platform() {
            Some(StandalonePlatform::Unix) => Some(UpdateAction::StandaloneUnix),
            Some(StandalonePlatform::Windows) | None => None,
        }
    }

    /// Returns the list of command-line arguments for invoking the update.
    pub fn command_args(self) -> (&'static str, &'static [&'static str]) {
        match self {
            UpdateAction::StandaloneUnix => (
                "sh",
                &[
                    "-c",
                    "installer=$(mktemp) && cleanup() { rm -f \"$installer\"; } && trap cleanup EXIT HUP INT TERM && curl -fsSL https://github.com/dkropachev/codex/releases/latest/download/install.sh -o \"$installer\" && CODEX_NON_INTERACTIVE=1 sh \"$installer\"",
                ],
            ),
        }
    }

    /// Returns string representation of the command-line arguments for invoking the update.
    pub fn command_str(self) -> String {
        let (command, args) = self.command_args();
        shlex::try_join(std::iter::once(command).chain(args.iter().copied()))
            .unwrap_or_else(|_| format!("{command} {}", args.join(" ")))
    }
}

#[cfg(any(not(debug_assertions), test))]
#[cfg_attr(test, allow(dead_code))]
pub fn get_update_action() -> Option<UpdateAction> {
    UpdateAction::from_install_context(InstallContext::current())
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_install_context::CodexPackageLayout;
    use codex_install_context::InstallMethod;
    use codex_utils_absolute_path::AbsolutePathBuf;
    use pretty_assertions::assert_eq;
    use std::fs;

    #[cfg(unix)]
    #[test]
    fn only_managed_fork_unix_maps_to_update_action() -> std::io::Result<()> {
        let codex_home = tempfile::tempdir()?;
        let target = match (std::env::consts::OS, std::env::consts::ARCH) {
            ("linux", "x86_64") => "x86_64-unknown-linux-musl",
            ("linux", "aarch64") => "aarch64-unknown-linux-musl",
            ("macos", "x86_64") => "x86_64-apple-darwin",
            ("macos", "aarch64") => "aarch64-apple-darwin",
            platform => panic!("unsupported test platform: {platform:?}"),
        };
        let release_dir = codex_home.path().join(format!(
            "packages/standalone/releases/dkropachev-0.150.0-{target}"
        ));
        let bin_dir = release_dir.join("bin");
        fs::create_dir_all(&bin_dir)?;
        fs::write(
            release_dir.join("codex-package.json"),
            serde_json::json!({
                "layoutVersion": 1,
                "version": "0.150.0",
                "target": target,
                "variant": "codex",
                "entrypoint": "bin/codex",
                "resourcesDir": "codex-resources",
                "pathDir": "codex-path",
            })
            .to_string(),
        )?;
        let release_dir = AbsolutePathBuf::from_absolute_path(release_dir)?;
        let bin_dir = AbsolutePathBuf::from_absolute_path(bin_dir)?;
        let managed_unix = InstallContext {
            method: InstallMethod::Standalone {
                platform: StandalonePlatform::Unix,
                release_dir: release_dir.clone(),
                resources_dir: None,
            },
            package_layout: Some(CodexPackageLayout {
                package_dir: release_dir.clone(),
                bin_dir,
                resources_dir: None,
                path_dir: None,
            }),
        };

        assert_eq!(
            UpdateAction::from_install_context(&managed_unix),
            Some(UpdateAction::StandaloneUnix)
        );

        for method in [
            InstallMethod::Npm,
            InstallMethod::Bun,
            InstallMethod::Pnpm,
            InstallMethod::Brew,
            InstallMethod::Other,
        ] {
            assert_eq!(
                UpdateAction::from_install_context(&InstallContext {
                    method,
                    package_layout: None,
                }),
                None
            );
        }

        let mut managed_windows = managed_unix;
        managed_windows.method = InstallMethod::Standalone {
            platform: StandalonePlatform::Windows,
            release_dir,
            resources_dir: None,
        };
        assert_eq!(UpdateAction::from_install_context(&managed_windows), None);
        Ok(())
    }

    #[test]
    fn standalone_update_commands_rerun_latest_installer() {
        assert_eq!(
            UpdateAction::StandaloneUnix.command_args(),
            (
                "sh",
                &[
                    "-c",
                    "installer=$(mktemp) && cleanup() { rm -f \"$installer\"; } && trap cleanup EXIT HUP INT TERM && curl -fsSL https://github.com/dkropachev/codex/releases/latest/download/install.sh -o \"$installer\" && CODEX_NON_INTERACTIVE=1 sh \"$installer\""
                ][..],
            )
        );
    }
}

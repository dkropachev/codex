use std::collections::BTreeSet;
use std::env;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::fs;
use std::fs::OpenOptions;
use std::io::Cursor;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use flate2::read::GzDecoder;
use reqwest::blocking::Client;
use sha2::Digest;
use sha2::Sha256;
use tar::Archive;

const BUN_VERSION: &str = "1.3.14";
const BUN_UNIX_ARCHIVE_ENTRY: &str = "package/bin/bun";
const BUN_WINDOWS_ARCHIVE_ENTRY: &str = "package/bin/bun.exe";
const WORKFLOW_BUN_BIN_DIR: &str = ".bin";
const WORKFLOW_BUN_ENV_DIR: &str = ".env";
const BUN_VERSION_FILE: &str = ".bun-version";
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
const INSTALL_LOCK_TIMEOUT: Duration = Duration::from_secs(180);
const INSTALL_LOCK_RETRY: Duration = Duration::from_millis(100);

static PREFETCHED_CACHE_ROOTS: OnceLock<Mutex<BTreeSet<PathBuf>>> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ManagedBunPackage {
    target: &'static str,
    archive_url: &'static str,
    archive_entry: &'static str,
    sha256: &'static str,
}

pub(crate) fn cached_managed_bun_path(cache_root: Option<&Path>) -> Result<Option<PathBuf>> {
    let Some(package) = current_package() else {
        return Ok(None);
    };
    let bun_path = managed_bun_path(cache_root)?;
    let version_path = managed_bun_version_path(cache_root)?;
    Ok(managed_bun_is_current(&bun_path, &version_path, package).then_some(bun_path))
}

pub(crate) fn ensure_managed_bun(cache_root: Option<&Path>) -> Result<Option<PathBuf>> {
    let Some(package) = current_package() else {
        return Ok(None);
    };

    let bun_path = managed_bun_path(cache_root)?;
    let version_path = managed_bun_version_path(cache_root)?;
    if managed_bun_is_current(&bun_path, &version_path, package) {
        return Ok(Some(bun_path));
    }

    let parent = bun_path.parent().ok_or_else(|| {
        anyhow!(
            "managed Bun path has no parent directory: {}",
            bun_path.display()
        )
    })?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create managed Bun directory {}",
            parent.display()
        )
    })?;
    let _install_lock = match acquire_install_lock(parent, &bun_path, &version_path, package)? {
        Some(lock) => lock,
        None => return Ok(Some(bun_path)),
    };
    if managed_bun_is_current(&bun_path, &version_path, package) {
        return Ok(Some(bun_path));
    }

    let archive = download_bun_archive(package)?;
    let tmp_path = parent.join(format!(
        ".bun-{}-{}.tmp",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let _ = fs::remove_file(&tmp_path);
    extract_bun_archive(package, &archive, &tmp_path)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o755)).with_context(|| {
            format!(
                "failed to make managed Bun executable {}",
                tmp_path.display()
            )
        })?;
    }

    let _ = fs::remove_file(&bun_path);
    fs::rename(&tmp_path, &bun_path).with_context(|| {
        format!(
            "failed to install managed Bun into cache {}",
            bun_path.display()
        )
    })?;
    fs::write(&version_path, managed_bun_version_marker(package)).with_context(|| {
        format!(
            "failed to write managed Bun version marker {}",
            version_path.display()
        )
    })?;

    Ok(Some(bun_path))
}

pub fn prefetch_managed_bun_runtime(cache_root: &Path) {
    let cache_root = cache_root.to_path_buf();
    let cache_roots = PREFETCHED_CACHE_ROOTS.get_or_init(|| Mutex::new(BTreeSet::new()));
    if let Ok(mut cache_roots) = cache_roots.lock()
        && !cache_roots.insert(cache_root.clone())
    {
        return;
    }
    thread::spawn(move || {
        let _ = ensure_isolated_bun_environment(Some(&cache_root));
        let _ = ensure_managed_bun(Some(&cache_root));
    });
}

pub(crate) fn prepend_managed_bun_to_path(
    command: &mut std::process::Command,
    cache_root: Option<&Path>,
) -> Result<()> {
    configure_isolated_bun_environment(command, cache_root)?;
    if let Some(bun_path) = cached_managed_bun_path(cache_root)? {
        prepend_to_path(command, &bun_path)?;
        return Ok(());
    }
    let Some(bun_path) = ensure_managed_bun(cache_root)? else {
        return Ok(());
    };
    prepend_to_path(command, &bun_path)
}

pub(crate) fn configure_isolated_bun_environment(
    command: &mut std::process::Command,
    cache_root: Option<&Path>,
) -> Result<()> {
    let environment = ensure_isolated_bun_environment(cache_root)?;
    environment.apply_to_std_command(command);
    Ok(())
}

pub(crate) fn configure_isolated_bun_environment_for_tokio(
    command: &mut tokio::process::Command,
    cache_root: Option<&Path>,
) -> Result<()> {
    let environment = ensure_isolated_bun_environment(cache_root)?;
    environment.apply_to_tokio_command(command);
    Ok(())
}

struct IsolatedBunEnvironment {
    bin_dir: PathBuf,
    env_dir: PathBuf,
    cache_dir: PathBuf,
    global_dir: PathBuf,
    transpiler_cache_dir: PathBuf,
}

impl IsolatedBunEnvironment {
    fn apply_to_std_command(&self, command: &mut std::process::Command) {
        command
            .env("BUN_INSTALL", &self.env_dir)
            .env("BUN_INSTALL_CACHE_DIR", &self.cache_dir)
            .env("BUN_INSTALL_GLOBAL_DIR", &self.global_dir)
            .env("BUN_INSTALL_BIN", &self.bin_dir)
            .env(
                "BUN_RUNTIME_TRANSPILER_CACHE_PATH",
                &self.transpiler_cache_dir,
            )
            .env_remove("BUN_OPTIONS");
    }

    fn apply_to_tokio_command(&self, command: &mut tokio::process::Command) {
        command
            .env("BUN_INSTALL", &self.env_dir)
            .env("BUN_INSTALL_CACHE_DIR", &self.cache_dir)
            .env("BUN_INSTALL_GLOBAL_DIR", &self.global_dir)
            .env("BUN_INSTALL_BIN", &self.bin_dir)
            .env(
                "BUN_RUNTIME_TRANSPILER_CACHE_PATH",
                &self.transpiler_cache_dir,
            )
            .env_remove("BUN_OPTIONS");
    }
}

fn ensure_isolated_bun_environment(cache_root: Option<&Path>) -> Result<IsolatedBunEnvironment> {
    let environment = isolated_bun_environment(cache_root)?;
    for dir in [
        environment.bin_dir.as_path(),
        environment.env_dir.as_path(),
        environment.cache_dir.as_path(),
        environment.global_dir.as_path(),
        environment.transpiler_cache_dir.as_path(),
    ] {
        fs::create_dir_all(dir).with_context(|| {
            format!(
                "failed to create isolated managed Bun directory {}",
                dir.display()
            )
        })?;
    }
    Ok(environment)
}

fn isolated_bun_environment(cache_root: Option<&Path>) -> Result<IsolatedBunEnvironment> {
    let workflows_dir = managed_bun_root(cache_root)?.join("workflows");
    let bin_dir = workflows_dir.join(WORKFLOW_BUN_BIN_DIR);
    let env_dir = workflows_dir.join(WORKFLOW_BUN_ENV_DIR);
    Ok(IsolatedBunEnvironment {
        bin_dir,
        cache_dir: env_dir.join("install").join("cache"),
        global_dir: env_dir.join("install").join("global"),
        transpiler_cache_dir: env_dir.join("runtime-transpiler-cache"),
        env_dir,
    })
}

fn prepend_to_path(command: &mut std::process::Command, bun_path: &Path) -> Result<()> {
    let Some(bin_dir) = bun_path.parent() else {
        return Ok(());
    };

    let mut paths = vec![bin_dir.to_path_buf()];
    if let Some(path) = command_env_os(command, "PATH") {
        paths.extend(env::split_paths(&path));
    }
    let paths = env::join_paths(paths).context("failed to build PATH for managed Bun")?;
    command.env("PATH", paths);
    Ok(())
}

fn command_env_os(command: &std::process::Command, key: &str) -> Option<OsString> {
    let key = OsStr::new(key);
    command
        .get_envs()
        .find_map(|(env_key, value)| (env_key == key).then(|| value.map(OsString::from)))
        .flatten()
        .or_else(|| env::var_os(key))
}

struct InstallLock {
    path: PathBuf,
}

impl Drop for InstallLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn acquire_install_lock(
    parent: &Path,
    bun_path: &Path,
    version_path: &Path,
    package: ManagedBunPackage,
) -> Result<Option<InstallLock>> {
    let lock_path = parent.join(".bun-install.lock");
    let deadline = Instant::now() + INSTALL_LOCK_TIMEOUT;
    loop {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(mut lock_file) => {
                let _ = writeln!(lock_file, "pid={}", std::process::id());
                return Ok(Some(InstallLock { path: lock_path }));
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                if managed_bun_is_current(bun_path, version_path, package) {
                    return Ok(None);
                }
                if Instant::now() >= deadline {
                    let _ = fs::remove_file(&lock_path);
                    continue;
                }
                thread::sleep(INSTALL_LOCK_RETRY);
            }
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "failed to lock managed Bun install at {}",
                        lock_path.display()
                    )
                });
            }
        }
    }
}

fn managed_bun_path(cache_root: Option<&Path>) -> Result<PathBuf> {
    Ok(managed_bun_root(cache_root)?
        .join("workflows")
        .join(WORKFLOW_BUN_BIN_DIR)
        .join(bun_executable_name()))
}

fn managed_bun_version_path(cache_root: Option<&Path>) -> Result<PathBuf> {
    Ok(managed_bun_root(cache_root)?
        .join("workflows")
        .join(WORKFLOW_BUN_BIN_DIR)
        .join(BUN_VERSION_FILE))
}

fn managed_bun_root(cache_root: Option<&Path>) -> Result<PathBuf> {
    let root = match cache_root {
        Some(cache_root) => cache_root.to_path_buf(),
        None => codex_utils_home_dir::find_codex_home()
            .context("failed to resolve CODEX_HOME for managed Bun")?
            .into_path_buf(),
    };
    Ok(root)
}

fn managed_bun_is_current(
    bun_path: &Path,
    version_path: &Path,
    package: ManagedBunPackage,
) -> bool {
    bun_path.is_file()
        && fs::read_to_string(version_path)
            .ok()
            .is_some_and(|version| version == managed_bun_version_marker(package))
}

fn managed_bun_version_marker(package: ManagedBunPackage) -> String {
    format!("{BUN_VERSION}\n{}\n", package.target)
}

fn bun_executable_name() -> &'static str {
    if cfg!(windows) { "bun.exe" } else { "bun" }
}

fn current_package() -> Option<ManagedBunPackage> {
    package_for_target(std::env::consts::OS, std::env::consts::ARCH)
}

fn package_for_target(target_os: &str, target_arch: &str) -> Option<ManagedBunPackage> {
    match (target_os, target_arch) {
        ("linux", "x86_64") => Some(ManagedBunPackage {
            target: "linux-x64-baseline",
            archive_url: "https://registry.npmjs.org/@oven/bun-linux-x64-baseline/-/bun-linux-x64-baseline-1.3.14.tgz",
            archive_entry: BUN_UNIX_ARCHIVE_ENTRY,
            sha256: "1d58ab332bf81a31ef3d59d0ddaf2d60e8889b7da9e6a41762492bf5675a2be5",
        }),
        ("linux", "aarch64") => Some(ManagedBunPackage {
            target: "linux-aarch64",
            archive_url: "https://registry.npmjs.org/@oven/bun-linux-aarch64/-/bun-linux-aarch64-1.3.14.tgz",
            archive_entry: BUN_UNIX_ARCHIVE_ENTRY,
            sha256: "97631ecfb616c248a4662599c555a59e2a18140a2ec1c0038a89bff08b815169",
        }),
        ("macos", "x86_64") => Some(ManagedBunPackage {
            target: "darwin-x64",
            archive_url: "https://registry.npmjs.org/@oven/bun-darwin-x64/-/bun-darwin-x64-1.3.14.tgz",
            archive_entry: BUN_UNIX_ARCHIVE_ENTRY,
            sha256: "1a0ca6b839a1243b2a857c63e6cdb7cee1eeacf538736a27bfb08e75a0789efa",
        }),
        ("macos", "aarch64") => Some(ManagedBunPackage {
            target: "darwin-aarch64",
            archive_url: "https://registry.npmjs.org/@oven/bun-darwin-aarch64/-/bun-darwin-aarch64-1.3.14.tgz",
            archive_entry: BUN_UNIX_ARCHIVE_ENTRY,
            sha256: "603d327a393c32fec5d9e7165c5f57afc28f1c84ef85593448870ccc41bda636",
        }),
        ("windows", "x86_64") => Some(ManagedBunPackage {
            target: "windows-x64",
            archive_url: "https://registry.npmjs.org/@oven/bun-windows-x64/-/bun-windows-x64-1.3.14.tgz",
            archive_entry: BUN_WINDOWS_ARCHIVE_ENTRY,
            sha256: "8e9c259ada7e1d3236a0e8c3fb644ba7d3214906fcc38f502e5422063eeac91b",
        }),
        _ => None,
    }
}

fn download_bun_archive(package: ManagedBunPackage) -> Result<Vec<u8>> {
    thread::spawn(move || download_bun_archive_blocking(package))
        .join()
        .map_err(|_| anyhow!("managed Bun download thread panicked"))?
}

fn download_bun_archive_blocking(package: ManagedBunPackage) -> Result<Vec<u8>> {
    let client = Client::builder()
        .timeout(DOWNLOAD_TIMEOUT)
        .build()
        .context("failed to build HTTP client for managed Bun download")?;
    let response = client
        .get(package.archive_url)
        .send()
        .with_context(|| format!("failed to download Bun from {}", package.archive_url))?
        .error_for_status()
        .with_context(|| format!("failed to download Bun from {}", package.archive_url))?;
    let archive = response
        .bytes()
        .context("failed to read managed Bun archive response")?;
    let digest = format!("{:x}", Sha256::digest(&archive));
    if digest != package.sha256 {
        anyhow::bail!(
            "managed Bun archive checksum mismatch: expected {}, got {}",
            package.sha256,
            digest
        );
    }
    Ok(archive.to_vec())
}

fn extract_bun_archive(
    package: ManagedBunPackage,
    archive: &[u8],
    destination: &Path,
) -> Result<()> {
    let decoder = GzDecoder::new(Cursor::new(archive));
    let mut archive = Archive::new(decoder);

    for entry in archive
        .entries()
        .context("failed to read managed Bun archive")?
    {
        let mut entry = entry.context("failed to read managed Bun archive entry")?;
        let path = entry
            .path()
            .context("failed to read managed Bun archive entry path")?;
        if path.as_ref() != Path::new(package.archive_entry) {
            continue;
        }

        entry.unpack(destination).with_context(|| {
            format!(
                "failed to extract managed Bun binary to {}",
                destination.display()
            )
        })?;
        return Ok(());
    }

    Err(anyhow!(
        "managed Bun archive did not contain {}",
        package.archive_entry
    ))
}

#[cfg(all(test, target_os = "linux", target_arch = "x86_64"))]
mod tests {
    use std::ffi::OsStr;
    use std::ffi::OsString;
    use std::fs;
    use std::process::Command;

    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    use super::cached_managed_bun_path;
    use super::configure_isolated_bun_environment;
    use super::ensure_managed_bun;
    use super::managed_bun_version_marker;
    use super::package_for_target;

    #[test]
    fn managed_bun_uses_cached_runtime_without_download() {
        let temp_dir = TempDir::new().unwrap();
        let bun = temp_dir.path().join("workflows/.bin/bun");
        let version = temp_dir.path().join("workflows/.bin/.bun-version");
        fs::create_dir_all(bun.parent().unwrap()).unwrap();
        fs::write(&bun, "cached").unwrap();
        fs::write(
            &version,
            managed_bun_version_marker(package_for_target("linux", "x86_64").unwrap()),
        )
        .unwrap();

        assert_eq!(
            cached_managed_bun_path(Some(temp_dir.path())).unwrap(),
            Some(bun.clone())
        );
        assert_eq!(
            ensure_managed_bun(Some(temp_dir.path())).unwrap(),
            Some(bun)
        );
    }

    #[test]
    fn managed_bun_selects_supported_packages() {
        let targets = [
            ("linux", "x86_64", "linux-x64-baseline"),
            ("linux", "aarch64", "linux-aarch64"),
            ("macos", "x86_64", "darwin-x64"),
            ("macos", "aarch64", "darwin-aarch64"),
            ("windows", "x86_64", "windows-x64"),
        ];

        for (target_os, target_arch, expected_target) in targets {
            assert_eq!(
                package_for_target(target_os, target_arch)
                    .expect("target should be supported")
                    .target,
                expected_target
            );
        }
    }

    #[test]
    fn managed_bun_rejects_unsupported_packages() {
        assert_eq!(package_for_target("linux", "riscv64"), None);
        assert_eq!(package_for_target("windows", "aarch64"), None);
    }

    #[test]
    fn isolated_bun_environment_uses_workflow_env_dir() {
        let temp_dir = TempDir::new().unwrap();
        let mut command = Command::new("bun");
        command.env("BUN_OPTIONS", "--inspect");

        configure_isolated_bun_environment(&mut command, Some(temp_dir.path())).unwrap();

        assert_eq!(
            command_env(&command, "BUN_INSTALL"),
            Some(temp_dir.path().join("workflows/.env").into_os_string())
        );
        assert_eq!(
            command_env(&command, "BUN_INSTALL_CACHE_DIR"),
            Some(
                temp_dir
                    .path()
                    .join("workflows/.env/install/cache")
                    .into_os_string()
            )
        );
        assert_eq!(
            command_env(&command, "BUN_INSTALL_BIN"),
            Some(temp_dir.path().join("workflows/.bin").into_os_string())
        );
        assert_eq!(
            command_env(&command, "BUN_RUNTIME_TRANSPILER_CACHE_PATH"),
            Some(
                temp_dir
                    .path()
                    .join("workflows/.env/runtime-transpiler-cache")
                    .into_os_string()
            )
        );
        assert_eq!(command_env(&command, "BUN_OPTIONS"), None);
        assert!(
            temp_dir
                .path()
                .join("workflows/.env/install/cache")
                .is_dir()
        );
        assert!(temp_dir.path().join("workflows/.bin").is_dir());
    }

    fn command_env(command: &Command, key: &str) -> Option<OsString> {
        let key = OsStr::new(key);
        command
            .get_envs()
            .find_map(|(env_key, value)| (env_key == key).then(|| value.map(OsString::from)))
            .flatten()
    }
}

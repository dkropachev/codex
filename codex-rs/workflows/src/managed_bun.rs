use std::collections::BTreeSet;
use std::env;
#[cfg(windows)]
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
const BUN_ARCHIVE_ENTRY: &str = "package/bin/bun";
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
const INSTALL_LOCK_TIMEOUT: Duration = Duration::from_secs(180);
const INSTALL_LOCK_RETRY: Duration = Duration::from_millis(100);

static PREFETCHED_CACHE_ROOTS: OnceLock<Mutex<BTreeSet<PathBuf>>> = OnceLock::new();

#[derive(Clone, Copy)]
struct ManagedBunPackage {
    target: &'static str,
    archive_url: &'static str,
    sha256: &'static str,
}

pub(crate) fn cached_managed_bun_path(cache_root: Option<&Path>) -> Result<Option<PathBuf>> {
    let Some(package) = current_package() else {
        return Ok(None);
    };
    let bun_path = managed_bun_path(cache_root, package)?;
    Ok(bun_path.is_file().then_some(bun_path))
}

pub(crate) fn ensure_managed_bun(cache_root: Option<&Path>) -> Result<Option<PathBuf>> {
    let Some(package) = current_package() else {
        return Ok(None);
    };

    let bun_path = managed_bun_path(cache_root, package)?;
    if bun_path.is_file() {
        return Ok(Some(bun_path));
    }

    let parent = bun_path.parent().ok_or_else(|| {
        anyhow!(
            "managed Bun cache path has no parent directory: {}",
            bun_path.display()
        )
    })?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create managed Bun cache directory {}",
            parent.display()
        )
    })?;
    let _install_lock = match acquire_install_lock(parent, &bun_path)? {
        Some(lock) => lock,
        None => return Ok(Some(bun_path)),
    };
    if bun_path.is_file() {
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
    extract_bun_archive(&archive, &tmp_path)?;

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

    fs::rename(&tmp_path, &bun_path).with_context(|| {
        format!(
            "failed to install managed Bun into cache {}",
            bun_path.display()
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
        let _ = ensure_managed_bun(Some(&cache_root));
    });
}

pub(crate) fn prepend_managed_bun_to_path(
    command: &mut std::process::Command,
    cache_root: Option<&Path>,
) -> Result<()> {
    if let Some(bun_path) = cached_managed_bun_path(cache_root)? {
        prepend_to_path(command, &bun_path)?;
        return Ok(());
    }
    if command_on_path("bun") {
        return Ok(());
    }
    let Some(bun_path) = ensure_managed_bun(cache_root)? else {
        return Ok(());
    };
    prepend_to_path(command, &bun_path)
}

pub(crate) fn command_on_path(command: &str) -> bool {
    let Some(paths) = env::var_os("PATH") else {
        return false;
    };

    env::split_paths(&paths).any(|dir| {
        if dir.join(command).is_file() {
            return true;
        }

        #[cfg(windows)]
        {
            let extensions =
                env::var_os("PATHEXT").unwrap_or_else(|| OsString::from(".COM;.EXE;.BAT;.CMD"));
            for extension in env::split_paths(&extensions) {
                let extension = extension.as_os_str().to_string_lossy();
                let extension = extension.trim_start_matches('.');
                if dir.join(format!("{command}.{extension}")).is_file() {
                    return true;
                }
            }
        }

        false
    })
}

fn prepend_to_path(command: &mut std::process::Command, bun_path: &Path) -> Result<()> {
    let Some(bin_dir) = bun_path.parent() else {
        return Ok(());
    };

    let mut paths = vec![bin_dir.to_path_buf()];
    if let Some(path) = env::var_os("PATH") {
        paths.extend(env::split_paths(&path));
    }
    let paths = env::join_paths(paths).context("failed to build PATH for managed Bun")?;
    command.env("PATH", paths);
    Ok(())
}

struct InstallLock {
    path: PathBuf,
}

impl Drop for InstallLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn acquire_install_lock(parent: &Path, bun_path: &Path) -> Result<Option<InstallLock>> {
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
                if bun_path.is_file() {
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

fn managed_bun_path(cache_root: Option<&Path>, package: ManagedBunPackage) -> Result<PathBuf> {
    let root = match cache_root {
        Some(cache_root) => cache_root.to_path_buf(),
        None => codex_utils_home_dir::find_codex_home()
            .context("failed to resolve CODEX_HOME for managed Bun cache")?
            .into_path_buf(),
    };
    Ok(root
        .join("workflows")
        .join("runtime")
        .join("bun")
        .join(BUN_VERSION)
        .join(package.target)
        .join(bun_executable_name()))
}

fn bun_executable_name() -> &'static str {
    if cfg!(windows) { "bun.exe" } else { "bun" }
}

fn current_package() -> Option<ManagedBunPackage> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
    {
        Some(ManagedBunPackage {
            target: "linux-x64-baseline",
            archive_url: "https://registry.npmjs.org/@oven/bun-linux-x64-baseline/-/bun-linux-x64-baseline-1.3.14.tgz",
            sha256: "1d58ab332bf81a31ef3d59d0ddaf2d60e8889b7da9e6a41762492bf5675a2be5",
        })
    }

    #[cfg(not(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu")))]
    {
        None
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

fn extract_bun_archive(archive: &[u8], destination: &Path) -> Result<()> {
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
        if path.as_ref() != Path::new(BUN_ARCHIVE_ENTRY) {
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
        "managed Bun archive did not contain {BUN_ARCHIVE_ENTRY}"
    ))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::BUN_VERSION;
    use super::cached_managed_bun_path;
    use super::ensure_managed_bun;

    #[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
    #[test]
    fn managed_bun_uses_cached_runtime_without_download() {
        let temp_dir = TempDir::new().unwrap();
        let bun = temp_dir
            .path()
            .join("workflows/runtime/bun")
            .join(BUN_VERSION)
            .join("linux-x64-baseline")
            .join("bun");
        fs::create_dir_all(bun.parent().unwrap()).unwrap();
        fs::write(&bun, "cached").unwrap();

        assert_eq!(
            cached_managed_bun_path(Some(temp_dir.path())).unwrap(),
            Some(bun.clone())
        );
        assert_eq!(
            ensure_managed_bun(Some(temp_dir.path())).unwrap(),
            Some(bun)
        );
    }
}

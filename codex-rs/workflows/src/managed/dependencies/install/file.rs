use std::fs;
use std::io::Read;
use std::path::Path;

use anyhow::Context;
use anyhow::bail;
use codex_utils_absolute_path::AbsolutePathBuf;

pub(super) fn read_bounded_regular_file(
    path: &AbsolutePathBuf,
    maximum_bytes: u64,
) -> anyhow::Result<Vec<u8>> {
    let file = open_no_follow(path.as_path()).with_context(|| {
        format!(
            "managed path {} must be a regular file without aliases",
            path.as_path().display()
        )
    })?;
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to inspect {}", path.as_path().display()))?;
    validate_regular_file(path.as_path(), &metadata)?;
    if !is_disk_file(&file) {
        bail!(
            "managed path {} must be a regular file without aliases",
            path.as_path().display()
        );
    }

    let mut contents = Vec::new();
    file.take(maximum_bytes + 1)
        .read_to_end(&mut contents)
        .with_context(|| format!("failed to read {}", path.as_path().display()))?;
    if contents.len() as u64 > maximum_bytes {
        bail!(
            "{} exceeds the {maximum_bytes}-byte limit",
            path.as_path().display()
        );
    }
    Ok(contents)
}

pub(super) fn open_no_follow(path: &Path) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use rustix::fs::OFlags;
        use std::os::unix::fs::OpenOptionsExt;

        options.custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC).bits() as i32);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        use windows_sys::Win32::Storage::FileSystem::SECURITY_IDENTIFICATION;

        options
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .security_qos_flags(SECURITY_IDENTIFICATION);
    }
    options.open(path)
}

fn validate_regular_file(path: &Path, metadata: &fs::Metadata) -> anyhow::Result<()> {
    if metadata.file_type().is_symlink()
        || is_windows_reparse_point(metadata)
        || !metadata.is_file()
    {
        bail!(
            "managed path {} must be a regular file without aliases",
            path.display()
        );
    }
    Ok(())
}

#[cfg(windows)]
fn is_disk_file(file: &fs::File) -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::FILE_TYPE_DISK;
    use windows_sys::Win32::Storage::FileSystem::GetFileType;

    // SAFETY: `file` owns this handle for the duration of the call.
    unsafe { GetFileType(file.as_raw_handle() as HANDLE) == FILE_TYPE_DISK }
}

#[cfg(not(windows))]
fn is_disk_file(_file: &fs::File) -> bool {
    true
}

pub(super) fn validate_directory_component(
    path: &Path,
    metadata: &fs::Metadata,
) -> anyhow::Result<()> {
    if !metadata.is_dir() || metadata.file_type().is_symlink() || is_windows_reparse_point(metadata)
    {
        bail!(
            "managed path component {} must be a regular directory without aliases",
            path.display()
        );
    }
    Ok(())
}

#[cfg(windows)]
pub(super) fn is_windows_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
pub(super) fn is_windows_reparse_point(_metadata: &fs::Metadata) -> bool {
    false
}

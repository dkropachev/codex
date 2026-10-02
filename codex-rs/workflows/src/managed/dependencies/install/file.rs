use std::fs;
use std::path::Path;

use anyhow::bail;

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
fn is_windows_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_windows_reparse_point(_metadata: &fs::Metadata) -> bool {
    false
}

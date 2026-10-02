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
    let metadata = fs::symlink_metadata(path.as_path())
        .with_context(|| format!("failed to inspect {}", path.as_path().display()))?;
    validate_regular_file(path.as_path(), &metadata)?;
    if metadata.len() > maximum_bytes {
        bail!(
            "{} exceeds the {maximum_bytes}-byte limit",
            path.as_path().display()
        );
    }

    let file = fs::File::open(path.as_path())
        .with_context(|| format!("failed to read {}", path.as_path().display()))?;
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

fn validate_regular_file(path: &Path, metadata: &fs::Metadata) -> anyhow::Result<()> {
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || is_windows_reparse_point(metadata)
    {
        bail!(
            "managed path {} must be a regular file without aliases",
            path.display()
        );
    }
    Ok(())
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
fn is_windows_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_windows_reparse_point(_metadata: &fs::Metadata) -> bool {
    false
}

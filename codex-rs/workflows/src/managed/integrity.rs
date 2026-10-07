use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::path::Component;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use anyhow::Context;
use anyhow::bail;
use sha2::Digest;
use sha2::Sha256;

use super::fetch::VerificationLimits;

const MAX_GIT_INDEX_BYTES: u64 = 32 * 1024 * 1024;

mod link;
mod verify;

use link::validate_dependency_link;
#[allow(unused_imports, reason = "consumed by the managed lifecycle stage")]
pub(in crate::managed) use verify::ActivationPayloadEvidence;
#[allow(unused_imports, reason = "consumed by the managed lifecycle stage")]
pub(in crate::managed) use verify::VerifiedWorkflowRelease;
#[allow(unused_imports, reason = "consumed by the managed lifecycle stage")]
pub(in crate::managed) use verify::verify_materialized_copy;
#[allow(unused_imports, reason = "consumed by the managed lifecycle stage")]
pub(in crate::managed) use verify::verify_post_install;
#[allow(unused_imports, reason = "consumed by the managed publication stage")]
pub(in crate::managed) use verify::verify_published_copy;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct IndexEntry {
    pub(super) path: String,
    pub(super) mode: String,
    pub(super) oid: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SourceIntegrityBaseline {
    pub(super) commit: String,
    pub(super) index: Vec<IndexEntry>,
    pub(super) index_sha256: [u8; 32],
    pub(super) source: PayloadInventory,
}

pub(super) fn capture_source_baseline(
    git: &OsStr,
    root: &Path,
    expected_commit: &str,
    limits: VerificationLimits,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<SourceIntegrityBaseline> {
    let working_directory = root.parent().context("workflow checkout has no parent")?;
    let mut head = super::fetch::repository_command(git, working_directory, root);
    head.args(["rev-parse", "--verify", "HEAD^{commit}"]);
    let head = super::fetch::run_git(head, "Git workflow baseline HEAD", cancelled)?;
    let head = std::str::from_utf8(&head)
        .context("Git HEAD was not UTF-8")?
        .trim();
    if !head.eq_ignore_ascii_case(expected_commit) {
        bail!("workflow checkout HEAD changed before baseline capture");
    }
    let mut index = super::fetch::repository_command(git, working_directory, root);
    index.args(["ls-files", "--stage", "-z", "--cached", "--full-name"]);
    let index = super::fetch::run_git(index, "Git workflow index baseline", cancelled)?;
    let index = parse_index(&index, expected_commit.len())?;
    let source = scan_payload(
        root,
        PayloadKind::Source,
        limits,
        crate::runner::CommandDeadline::after(Duration::from_secs(/*secs*/ 60)),
        cancelled,
    )?;
    let files = source
        .entries
        .iter()
        .filter_map(|entry| match entry.kind {
            PayloadEntryKind::File { executable, .. } => Some((&entry.path, executable)),
            PayloadEntryKind::Directory | PayloadEntryKind::DependencyLink { .. } => None,
        })
        .collect::<Vec<_>>();
    if files.len() != index.len()
        || files
            .iter()
            .zip(&index)
            .any(|((path, executable), indexed)| {
                *path != &indexed.path || unix_mode_mismatch(*executable, &indexed.mode)
            })
    {
        bail!("workflow checkout files or executable modes do not match Git index");
    }
    let mut diff = super::fetch::repository_command(git, working_directory, root);
    diff.args(["diff", "--quiet", "--no-ext-diff", "--no-textconv", "--"]);
    super::fetch::run_git(diff, "Git workflow source baseline", cancelled)?;
    let mut staged_diff = super::fetch::repository_command(git, working_directory, root);
    staged_diff.args([
        "diff",
        "--cached",
        "--quiet",
        "--no-ext-diff",
        "--no-textconv",
        "HEAD",
        "--",
    ]);
    super::fetch::run_git(staged_diff, "Git workflow index baseline", cancelled)?;
    let index_sha256 = hash_regular_file(
        &root.join(".git/index"),
        MAX_GIT_INDEX_BYTES,
        crate::runner::CommandDeadline::after(Duration::from_secs(/*secs*/ 60)),
        cancelled,
    )?
    .0;
    Ok(SourceIntegrityBaseline {
        commit: expected_commit.to_ascii_lowercase(),
        index,
        index_sha256,
        source,
    })
}

#[cfg(unix)]
fn unix_mode_mismatch(executable: bool, mode: &str) -> bool {
    executable != (mode == "100755")
}

#[cfg(windows)]
fn unix_mode_mismatch(_executable: bool, _mode: &str) -> bool {
    false
}

fn parse_index(bytes: &[u8], oid_length: usize) -> anyhow::Result<Vec<IndexEntry>> {
    if !bytes.is_empty() && bytes.last() != Some(&0) {
        bail!("Git index output was unterminated");
    }
    let mut entries = Vec::new();
    for record in bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let separator = record
            .iter()
            .position(|byte| *byte == b'\t')
            .context("malformed Git index entry")?;
        let metadata =
            std::str::from_utf8(&record[..separator]).context("invalid Git index metadata")?;
        let mut parts = metadata.split(' ');
        let (Some(mode), Some(oid), Some("0"), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            bail!("Git index contains a malformed or unmerged entry");
        };
        if !matches!(mode, "100644" | "100755")
            || oid.len() != oid_length
            || !oid.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            bail!("Git index contains an unsupported entry");
        }
        let path = std::str::from_utf8(&record[separator + 1..])
            .context("Git index path must be UTF-8")?;
        super::fetch::portable_path(path)?;
        entries.push(IndexEntry {
            path: path.to_owned(),
            mode: mode.to_owned(),
            oid: oid.to_ascii_lowercase(),
        });
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    if entries.windows(2).any(|pair| pair[0].path == pair[1].path) {
        bail!("Git index contains duplicate paths");
    }
    Ok(entries)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PayloadKind {
    Source,
    Staged,
    Installed,
    Published,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum PayloadEntryKind {
    Directory,
    File { executable: bool, sha256: [u8; 32] },
    DependencyLink { target: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PayloadEntry {
    pub(super) path: String,
    pub(super) kind: PayloadEntryKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PayloadInventory {
    pub(super) entries: Vec<PayloadEntry>,
    pub(super) logical_bytes: u64,
}

pub(super) fn scan_payload(
    root: &Path,
    kind: PayloadKind,
    limits: VerificationLimits,
    deadline: crate::runner::CommandDeadline,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<PayloadInventory> {
    let root_metadata =
        fs::symlink_metadata(root).context("failed to inspect workflow payload root")?;
    if !root_metadata.is_dir()
        || root_metadata.file_type().is_symlink()
        || is_windows_reparse_point(&root_metadata)
    {
        bail!("workflow payload root must be a regular directory");
    }
    let mut pending = vec![root.to_path_buf()];
    let mut entries = Vec::new();
    let mut logical_bytes = 0_u64;
    let mut portable_paths = BTreeMap::new();
    while let Some(directory) = pending.pop() {
        deadline.check(cancelled)?;
        for entry in
            fs::read_dir(&directory).context("failed to read workflow payload directory")?
        {
            deadline.check(cancelled)?;
            let entry = entry.context("failed to read workflow payload entry")?;
            if directory == root
                && entry.file_name() == ".git"
                && !matches!(kind, PayloadKind::Installed | PayloadKind::Published)
            {
                continue;
            }
            if directory == root
                && entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.eq_ignore_ascii_case("codex-managed-workflow"))
            {
                if kind == PayloadKind::Published && entry.file_name() == "codex-managed-workflow" {
                    continue;
                }
                bail!("workflow payload contains reserved management marker");
            }
            if directory == root
                && entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.eq_ignore_ascii_case("node_modules"))
                && kind == PayloadKind::Source
            {
                bail!("workflow source already contains node_modules");
            }
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .context("payload entry escaped root")?;
            let portable = portable_path(relative)?;
            let mut prefix = String::new();
            for component in portable.split('/') {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(component);
                let normalized = prefix.to_ascii_lowercase();
                if portable_paths
                    .insert(normalized, prefix.clone())
                    .is_some_and(|previous| previous != prefix)
                {
                    bail!("workflow payload contains paths that collide on supported platforms");
                }
            }
            let count = entries
                .len()
                .checked_add(1)
                .context("workflow entry count overflow")?;
            if count > limits.post_install_entries {
                bail!(
                    "workflow payload exceeds {} entries",
                    limits.post_install_entries
                );
            }
            let metadata = fs::symlink_metadata(&path)
                .with_context(|| format!("failed to inspect workflow payload entry {portable}"))?;
            let entry_kind = if metadata.file_type().is_symlink() {
                if kind == PayloadKind::Source
                    || relative == Path::new("node_modules")
                    || !relative.starts_with("node_modules")
                {
                    bail!("workflow payload contains an unsupported symbolic link");
                }
                let target = fs::read_link(&path).context("failed to read dependency link")?;
                validate_dependency_link(&root.join("node_modules"), &path, &target)?;
                let target = target
                    .to_str()
                    .context("dependency link target must be UTF-8")?;
                logical_bytes = add_bytes(logical_bytes, target.len() as u64, limits)?;
                PayloadEntryKind::DependencyLink {
                    target: target.to_owned(),
                }
            } else if is_windows_reparse_point(&metadata) {
                bail!("workflow payload contains a reparse point");
            } else if metadata.is_dir() {
                pending.push(path);
                PayloadEntryKind::Directory
            } else if metadata.is_file() {
                let remaining_bytes = limits
                    .post_install_bytes
                    .checked_sub(logical_bytes)
                    .context("workflow payload byte count overflow")?;
                let (sha256, size) =
                    hash_regular_file(&path, remaining_bytes, deadline, cancelled)?;
                logical_bytes = add_bytes(logical_bytes, size, limits)?;
                PayloadEntryKind::File {
                    executable: is_executable(&metadata),
                    sha256,
                }
            } else {
                bail!("workflow payload contains a special file");
            };
            entries.push(PayloadEntry {
                path: portable,
                kind: entry_kind,
            });
        }
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(PayloadInventory {
        entries,
        logical_bytes,
    })
}

fn add_bytes(current: u64, added: u64, limits: VerificationLimits) -> anyhow::Result<u64> {
    let bytes = current
        .checked_add(added)
        .context("workflow payload byte count overflow")?;
    if bytes > limits.post_install_bytes {
        bail!(
            "workflow payload exceeds {} logical bytes",
            limits.post_install_bytes
        );
    }
    Ok(bytes)
}

fn portable_path(path: &Path) -> anyhow::Result<String> {
    let mut components = Vec::new();
    for component in path.components() {
        let Component::Normal(component) = component else {
            bail!("workflow payload contains an unsafe path");
        };
        components.push(
            component
                .to_str()
                .context("workflow payload path must be UTF-8")?,
        );
    }
    let portable = components.join("/");
    super::fetch::portable_path(&portable)?;
    Ok(portable)
}

fn hash_regular_file(
    path: &Path,
    maximum_bytes: u64,
    deadline: crate::runner::CommandDeadline,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<([u8; 32], u64)> {
    let mut file = open_no_follow(path).context("failed to open regular workflow file")?;
    let metadata = file
        .metadata()
        .context("failed to inspect opened workflow file")?;
    if !metadata.is_file() || is_windows_reparse_point(&metadata) {
        bail!("workflow payload file changed type during inspection");
    }
    if metadata.len() > maximum_bytes {
        bail!("workflow payload exceeds its logical byte limit");
    }
    let mut hasher = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        deadline.check(cancelled)?;
        let read = file
            .read(&mut buffer)
            .context("failed to read workflow payload file")?;
        if read == 0 {
            break;
        }
        bytes = bytes
            .checked_add(read as u64)
            .context("workflow file size overflow")?;
        if bytes > maximum_bytes {
            bail!("workflow payload exceeds its logical byte limit");
        }
        hasher.update(&buffer[..read]);
    }
    if bytes != metadata.len() {
        bail!("workflow payload file changed size during inspection");
    }
    Ok((hasher.finalize().into(), bytes))
}

#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(windows)]
fn is_executable(_metadata: &fs::Metadata) -> bool {
    false
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

fn open_no_follow(path: &Path) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(
            (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32,
        );
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

#[cfg(test)]
#[path = "integrity_tests.rs"]
mod tests;

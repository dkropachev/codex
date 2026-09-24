use std::fs;
#[cfg(all(unix, not(target_os = "redox")))]
use std::io::Write;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use anyhow::Context;
use anyhow::bail;
use serde_json::json;

use crate::ValidationCommand;
use crate::ValidationCoverage;
use crate::ValidationPolicy;
use crate::WorkflowManifest;
use crate::manifest::MAX_PACKAGE_JSON_BYTES;
use crate::manifest::MAX_WORKFLOW_SOURCE_BYTES;
use crate::manifest::MAX_WORKFLOW_YAML_BYTES;
use crate::manifest::WORKFLOW_API_VERSION;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScaffoldRequest {
    pub id: String,
    pub title: String,
    pub callable_name: String,
    pub description: String,
}

pub fn scaffold_workflow(root: &Path, request: &ScaffoldRequest) -> anyhow::Result<PathBuf> {
    let id = normalize_workflow_id(&request.id)?;
    let id_components = id.split('/').collect::<Vec<_>>();
    validate_callable_name(&request.callable_name)?;
    if request.title.trim().is_empty() {
        bail!("workflow title must not be empty");
    }
    if request.description.trim().is_empty() {
        bail!("workflow description must not be empty");
    }
    let target = workflow_path(root, &id)?;
    reject_existing_or_symlink_path(root, &target)?;

    #[cfg(not(all(unix, not(target_os = "redox"))))]
    let parent = target
        .parent()
        .context("workflow target should have a parent directory")?;
    #[cfg(all(unix, not(target_os = "redox")))]
    let staging_parent = SecureWorkflowParent::open_or_create(
        root,
        &id_components[..id_components.len().saturating_sub(1)],
    )?;
    #[cfg(all(unix, not(target_os = "redox")))]
    {
        let mut staging = SecureStagingDirectory::create(&staging_parent.directory)?;
        write_package_at(&staging.directory, request, &id)?;
        initialize_git_repository_at(&staging.directory)?;
        let target_parent = SecureWorkflowParent::open_or_create(
            root,
            &id_components[..id_components.len().saturating_sub(1)],
        )?;
        staging.install_into(
            &target_parent.directory,
            id_components
                .last()
                .context("workflow id should have a final component")?,
        )?;
        Ok(target)
    }
    #[cfg(windows)]
    {
        let staging = tempfile::Builder::new()
            .prefix(".codex-workflow-")
            .tempdir()
            .context("failed to stage workflow package")?;
        write_package(staging.path(), request, &id)?;
        initialize_git_repository(staging.path())?;
        let _locked_parent = SecureWindowsPath::open_or_create(parent)?;
        reject_existing_or_symlink_path(root, &target)?;
        let staging_path = staging.keep();
        if let Err(err) = install_staged_package(&staging_path, &target) {
            let _ = fs::remove_dir_all(&staging_path);
            return Err(err).with_context(|| {
                format!(
                    "failed to install workflow package at {}; the target may have been created concurrently",
                    target.display()
                )
            });
        }
        Ok(target)
    }
    #[cfg(not(any(all(unix, not(target_os = "redox")), windows)))]
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create workflow parent {}", parent.display()))?;
        reject_existing_or_symlink_path(root, &target)?;
        let staging = tempfile::Builder::new()
            .prefix(".codex-workflow-")
            .tempdir_in(parent)
            .with_context(|| format!("failed to stage workflow under {}", parent.display()))?;
        write_package(staging.path(), request, &id)?;
        initialize_git_repository(staging.path())?;
        let staging_path = staging.keep();
        if let Err(err) = install_staged_package(&staging_path, &target) {
            let _ = fs::remove_dir_all(&staging_path);
            return Err(err).with_context(|| {
                format!(
                    "failed to install workflow package at {}; the target may have been created concurrently",
                    target.display()
                )
            });
        }
        Ok(target)
    }
}

/// Holds non-delete-sharing handles for every Windows parent component while a package is
/// installed, preventing a concurrent reparse-point swap from redirecting the final rename.
#[cfg(windows)]
struct SecureWindowsPath {
    _handles: Vec<std::os::windows::io::OwnedHandle>,
}

#[cfg(windows)]
impl SecureWindowsPath {
    fn open_or_create(path: &Path) -> anyhow::Result<Self> {
        use std::os::windows::ffi::OsStrExt;
        use std::os::windows::io::FromRawHandle;

        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::Storage::FileSystem::BY_HANDLE_FILE_INFORMATION;
        use windows_sys::Win32::Storage::FileSystem::CreateFileW;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_DIRECTORY;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        use windows_sys::Win32::Storage::FileSystem::FILE_READ_ATTRIBUTES;
        use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
        use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_WRITE;
        use windows_sys::Win32::Storage::FileSystem::GetFileInformationByHandle;
        use windows_sys::Win32::Storage::FileSystem::OPEN_EXISTING;

        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };
        let mut current = PathBuf::new();
        let mut handles = Vec::new();
        for component in absolute.components() {
            match component {
                Component::Prefix(_) | Component::RootDir => {
                    current.push(component.as_os_str());
                    continue;
                }
                Component::CurDir => continue,
                Component::ParentDir => {
                    bail!(
                        "workflow parent path must not contain '..': {}",
                        path.display()
                    );
                }
                Component::Normal(component) => current.push(component),
            }
            match fs::create_dir(&current) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(err) => {
                    return Err(err).with_context(|| {
                        format!("failed to create workflow directory {}", current.display())
                    });
                }
            }
            let wide = current
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect::<Vec<_>>();
            let raw = unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    FILE_READ_ATTRIBUTES,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                    0,
                )
            };
            if raw == INVALID_HANDLE_VALUE {
                return Err(std::io::Error::last_os_error()).with_context(|| {
                    format!("failed to lock workflow directory {}", current.display())
                });
            }
            let handle = unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(raw as _) };
            let mut information = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
            if unsafe { GetFileInformationByHandle(raw, information.as_mut_ptr()) } == 0 {
                return Err(std::io::Error::last_os_error()).with_context(|| {
                    format!("failed to inspect workflow directory {}", current.display())
                });
            }
            let attributes = unsafe { information.assume_init() }.dwFileAttributes;
            if attributes & FILE_ATTRIBUTE_DIRECTORY == 0
                || attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
            {
                bail!(
                    "workflow path component {} must be a directory and not a reparse point",
                    current.display()
                );
            }
            handles.push(handle);
        }
        Ok(Self { _handles: handles })
    }
}

#[cfg(all(unix, not(target_os = "redox")))]
#[derive(Debug)]
struct SecureWorkflowParent {
    directory: std::os::fd::OwnedFd,
}

#[cfg(all(unix, not(target_os = "redox")))]
impl SecureWorkflowParent {
    fn open_or_create(root: &Path, relative: &[&str]) -> anyhow::Result<Self> {
        use rustix::fs::Mode;
        use rustix::fs::OFlags;
        use rustix::fs::open;
        use rustix::fs::openat;

        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW;
        let (anchor, components): (&Path, Box<dyn Iterator<Item = std::path::Component<'_>>>) =
            if root.is_absolute() {
                (Path::new("/"), Box::new(root.components().skip(1)))
            } else {
                (Path::new("."), Box::new(root.components()))
            };
        let mut directory = open(anchor, flags, Mode::empty())
            .with_context(|| format!("failed to open workflow root anchor {}", anchor.display()))?;
        for component in components {
            match component {
                Component::Normal(component) => {
                    create_directory_at(&directory, component)?;
                    directory = openat(&directory, component, flags, Mode::empty()).map_err(|err| {
                        anyhow::anyhow!(
                            "workflow path component {} must be a directory and not a symbolic link: {err}",
                            component.to_string_lossy()
                        )
                    })?;
                }
                Component::CurDir => {}
                Component::ParentDir => {
                    directory = openat(&directory, "..", flags, Mode::empty())
                        .context("failed to traverse workflow root parent")?;
                }
                Component::Prefix(_) | Component::RootDir => {}
            }
        }
        for component in relative {
            create_directory_at(&directory, *component)?;
            directory = openat(&directory, *component, flags, Mode::empty()).map_err(|err| {
                anyhow::anyhow!(
                    "workflow path component {component} must be a directory and not a symbolic link: {err}"
                )
            })?;
        }
        Ok(Self { directory })
    }
}

#[cfg(all(unix, not(target_os = "redox")))]
fn create_directory_at(
    parent: &std::os::fd::OwnedFd,
    component: impl rustix::path::Arg,
) -> anyhow::Result<()> {
    use rustix::fs::Mode;
    use rustix::fs::mkdirat;
    use rustix::io::Errno;

    match mkdirat(parent, component, Mode::RWXU | Mode::RWXG | Mode::RWXO) {
        Ok(()) | Err(Errno::EXIST) => Ok(()),
        Err(err) => Err(err).context("failed to create workflow directory"),
    }
}

#[cfg(all(unix, not(target_os = "redox")))]
struct SecureStagingDirectory<'a> {
    parent: &'a std::os::fd::OwnedFd,
    name: std::ffi::OsString,
    directory: std::os::fd::OwnedFd,
    installed: bool,
}

#[cfg(all(unix, not(target_os = "redox")))]
impl<'a> SecureStagingDirectory<'a> {
    fn create(parent: &'a std::os::fd::OwnedFd) -> anyhow::Result<Self> {
        use std::sync::atomic::AtomicU64;
        use std::sync::atomic::Ordering;

        use rustix::fs::Mode;
        use rustix::fs::OFlags;
        use rustix::fs::mkdirat;
        use rustix::fs::openat;
        use rustix::io::Errno;

        static NEXT_STAGING_ID: AtomicU64 = AtomicU64::new(0);
        for _ in 0..128 {
            let sequence = NEXT_STAGING_ID.fetch_add(1, Ordering::Relaxed);
            let name = std::ffi::OsString::from(format!(
                ".codex-workflow-{}-{sequence}",
                std::process::id()
            ));
            match mkdirat(parent, &name, Mode::RWXU) {
                Ok(()) => {
                    let directory = openat(
                        parent,
                        &name,
                        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW,
                        Mode::empty(),
                    )
                    .context("failed to open workflow staging directory")?;
                    return Ok(Self {
                        parent,
                        name,
                        directory,
                        installed: false,
                    });
                }
                Err(Errno::EXIST) => continue,
                Err(err) => {
                    return Err(err).context("failed to create workflow staging directory");
                }
            }
        }
        bail!("failed to allocate a unique workflow staging directory");
    }

    fn install_into(
        &mut self,
        target_parent: &std::os::fd::OwnedFd,
        target_name: &str,
    ) -> anyhow::Result<()> {
        use rustix::fs::RenameFlags;
        use rustix::fs::renameat_with;

        renameat_with(
            self.parent,
            &self.name,
            target_parent,
            target_name,
            RenameFlags::NOREPLACE,
        )
        .context("failed to atomically install workflow package")?;
        self.installed = true;
        Ok(())
    }
}

#[cfg(all(unix, not(target_os = "redox")))]
impl Drop for SecureStagingDirectory<'_> {
    fn drop(&mut self) {
        if self.installed {
            return;
        }
        let _ = remove_directory_contents(&self.directory);
        let _ = rustix::fs::unlinkat(self.parent, &self.name, rustix::fs::AtFlags::REMOVEDIR);
    }
}

#[cfg(all(unix, not(target_os = "redox")))]
fn remove_directory_contents(directory: &std::os::fd::OwnedFd) -> anyhow::Result<()> {
    use rustix::fs::AtFlags;
    use rustix::fs::Dir;
    use rustix::fs::FileType;
    use rustix::fs::Mode;
    use rustix::fs::OFlags;
    use rustix::fs::openat;
    use rustix::fs::statat;
    use rustix::fs::unlinkat;

    let mut entries = Dir::read_from(directory).context("failed to read staging directory")?;
    while let Some(entry) = entries.read() {
        let entry = entry.context("failed to read staging directory entry")?;
        let name = entry.file_name();
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        let metadata = statat(directory, name, AtFlags::SYMLINK_NOFOLLOW)
            .context("failed to inspect staging entry")?;
        if FileType::from_raw_mode(metadata.st_mode) == FileType::Directory {
            let child = openat(
                directory,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW,
                Mode::empty(),
            )
            .context("failed to open staging subdirectory")?;
            remove_directory_contents(&child)?;
            unlinkat(directory, name, AtFlags::REMOVEDIR)
                .context("failed to remove staging subdirectory")?;
        } else {
            unlinkat(directory, name, AtFlags::empty()).context("failed to remove staging file")?;
        }
    }
    Ok(())
}

#[cfg(all(unix, not(target_os = "redox")))]
fn write_package_at(
    root: &std::os::fd::OwnedFd,
    request: &ScaffoldRequest,
    id: &str,
) -> anyhow::Result<()> {
    for file in package_files(request, id)? {
        write_file_at(root, Path::new(file.path), file.contents.as_bytes())?;
    }
    Ok(())
}

#[cfg(all(unix, not(target_os = "redox")))]
fn write_file_at(
    root: &std::os::fd::OwnedFd,
    relative: &Path,
    contents: &[u8],
) -> anyhow::Result<()> {
    use rustix::fs::Mode;
    use rustix::fs::OFlags;
    use rustix::fs::openat;

    let mut directory = openat(
        root,
        ".",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW,
        Mode::empty(),
    )?;
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(component) = component else {
            bail!(
                "generated package path must be relative: {}",
                relative.display()
            );
        };
        if components.peek().is_some() {
            create_directory_at(&directory, component)?;
            directory = openat(
                &directory,
                component,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW,
                Mode::empty(),
            )?;
        } else {
            let file = openat(
                &directory,
                component,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW, // codespell:ignore WRONLY
                Mode::RUSR | Mode::WUSR | Mode::RGRP | Mode::WGRP | Mode::ROTH | Mode::WOTH, // codespell:ignore WOTH
            )?;
            let mut file = fs::File::from(file);
            file.write_all(contents)?;
        }
    }
    Ok(())
}

#[cfg(all(unix, not(target_os = "redox")))]
fn initialize_git_repository_at(root: &std::os::fd::OwnedFd) -> anyhow::Result<()> {
    use std::os::fd::AsRawFd;
    use std::os::fd::BorrowedFd;
    use std::os::unix::process::CommandExt;

    let raw_fd = root.as_raw_fd();
    let mut command = Command::new("git");
    command.args(["init", "--quiet", "."]);
    unsafe {
        command.pre_exec(move || {
            let directory = BorrowedFd::borrow_raw(raw_fd);
            rustix::process::fchdir(directory).map_err(Into::into)
        });
    }
    let output = command
        .output()
        .context("failed to start git while initializing workflow package")?;
    if !output.status.success() {
        bail!(
            "git init failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(target_os = "redox")]
fn install_staged_package(staging: &Path, target: &Path) -> std::io::Result<()> {
    use rustix::fs::CWD;
    use rustix::fs::RenameFlags;
    use rustix::fs::renameat_with;

    renameat_with(CWD, staging, CWD, target, RenameFlags::NOREPLACE).map_err(Into::into)
}

#[cfg(windows)]
fn install_staged_package(staging: &Path, target: &Path) -> std::io::Result<()> {
    fs::rename(staging, target)
}

#[cfg(not(any(unix, windows)))]
fn install_staged_package(staging: &Path, target: &Path) -> std::io::Result<()> {
    fs::create_dir(target)?;
    let result = (|| {
        for entry in fs::read_dir(staging)? {
            let entry = entry?;
            fs::rename(entry.path(), target.join(entry.file_name()))?;
        }
        fs::remove_dir(staging)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(target);
    }
    result
}

pub fn workflow_path(root: &Path, id: &str) -> anyhow::Result<PathBuf> {
    let mut path = root.to_path_buf();
    for component in normalize_workflow_id(id)?.split('/') {
        path.push(component);
    }
    Ok(path)
}

pub fn normalize_workflow_id(raw: &str) -> anyhow::Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.contains('\\') || trimmed.split('/').any(str::is_empty) {
        bail!("workflow id is invalid: {raw}");
    }
    let path = Path::new(trimmed);
    if path.is_absolute() {
        bail!("workflow id must be relative: {raw}");
    }
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(component) => {
                let component = component.to_str().context("workflow id must be UTF-8")?;
                if component.is_empty()
                    || !component.chars().all(|ch| {
                        ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '-' | '_')
                    })
                {
                    bail!("workflow id component `{component}` is invalid");
                }
                components.push(component);
            }
            Component::CurDir | Component::ParentDir => {
                bail!("workflow id must not contain '.' or '..': {raw}");
            }
            Component::Prefix(_) | Component::RootDir => {
                bail!("workflow id must be relative: {raw}");
            }
        }
    }
    if components.is_empty() {
        bail!("workflow id is invalid: {raw}");
    }
    Ok(components.join("/"))
}

fn validate_callable_name(name: &str) -> anyhow::Result<()> {
    if !name
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
        || !name
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '-' | '_'))
    {
        bail!("workflow callable name is invalid: {name}");
    }
    Ok(())
}

fn reject_existing_or_symlink_path(root: &Path, target: &Path) -> anyhow::Result<()> {
    let mut root_prefix = PathBuf::new();
    for component in root.components() {
        root_prefix.push(component.as_os_str());
        if fs::symlink_metadata(&root_prefix)
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            bail!(
                "workflow root traverses symbolic link {}",
                root_prefix.display()
            );
        }
    }
    let relative = target
        .strip_prefix(root)
        .context("workflow target escaped its configured root")?;
    let mut candidate = root.to_path_buf();
    for component in relative.components() {
        candidate.push(component);
        match fs::symlink_metadata(&candidate) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!(
                    "workflow target traverses symbolic link {}",
                    candidate.display()
                );
            }
            Ok(_) if candidate == target => {
                bail!("workflow target {} already exists", target.display());
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("failed to inspect {}", candidate.display()));
            }
        }
    }
    Ok(())
}

#[cfg(not(all(unix, not(target_os = "redox"))))]
fn write_package(root: &Path, request: &ScaffoldRequest, id: &str) -> anyhow::Result<()> {
    fs::create_dir_all(root.join("src/tests"))?;
    fs::create_dir_all(root.join("state"))?;
    for file in package_files(request, id)? {
        fs::write(root.join(file.path), file.contents)?;
    }
    Ok(())
}

struct PackageFile {
    path: &'static str,
    contents: String,
}

fn package_files(request: &ScaffoldRequest, id: &str) -> anyhow::Result<Vec<PackageFile>> {
    let manifest = WorkflowManifest {
        api_version: WORKFLOW_API_VERSION,
        id: id.to_string(),
        title: request.title.clone(),
        callable_name: request.callable_name.clone(),
        description: request.description.clone(),
        validation: ValidationPolicy {
            commands: vec![ValidationCommand {
                program: "bun".to_string(),
                args: vec!["test".to_string()],
            }],
            coverage: ValidationCoverage {
                positive: true,
                load: true,
                autocomplete: true,
                negative: true,
                recovery: false,
            },
        },
    };
    let workflow_yaml = serde_yaml::to_string(&manifest)?;
    let package_json = format!(
        "{}\n",
        serde_json::to_string_pretty(&json!({
            "name": format!("@codex-workflow/{}", id.replace('/', "-")),
            "private": true,
            "type": "module",
            "scripts": { "test": "bun test" },
            "dependencies": {},
            "devDependencies": {},
        }))?
    );
    let workflow_source = workflow_source(request, id)?;
    if workflow_yaml.len() as u64 > MAX_WORKFLOW_YAML_BYTES {
        bail!("generated workflow.yaml exceeds the {MAX_WORKFLOW_YAML_BYTES}-byte limit");
    }
    if package_json.len() as u64 > MAX_PACKAGE_JSON_BYTES {
        bail!("generated package.json exceeds the {MAX_PACKAGE_JSON_BYTES}-byte limit");
    }
    if workflow_source.len() as u64 > MAX_WORKFLOW_SOURCE_BYTES {
        bail!("generated src/workflow.ts exceeds the {MAX_WORKFLOW_SOURCE_BYTES}-byte limit");
    }
    Ok(vec![
        PackageFile {
            path: "workflow.yaml",
            contents: workflow_yaml,
        },
        PackageFile {
            path: "package.json",
            contents: package_json,
        },
        PackageFile {
            path: ".gitignore",
            contents: "node_modules/\nartifacts/\nstate/*\n!state/.gitkeep\n".to_string(),
        },
        PackageFile {
            path: "README.md",
            contents: format!(
                "# {}\n\n{}\n\nInvoke this workflow as `/{}`.\n",
                request.title, request.description, request.callable_name
            ),
        },
        PackageFile {
            path: "DESIGN.md",
            contents: format!(
                "# {} design\n\n## Contract\n\nThe workflow accepts the explicit Draft 2020-12 input schema in `src/workflow.ts` and returns formatted `markdown.v1`.\n",
                request.title
            ),
        },
        PackageFile {
            path: "src/workflow.ts",
            contents: workflow_source,
        },
        PackageFile {
            path: "src/tests/workflow.test.ts",
            contents: workflow_test_source().to_string(),
        },
        PackageFile {
            path: "state/.gitkeep",
            contents: String::new(),
        },
    ])
}

fn workflow_source(request: &ScaffoldRequest, id: &str) -> anyhow::Result<String> {
    let id = serde_json::to_string(id)?;
    let title = serde_json::to_string(&request.title)?;
    let callable_name = serde_json::to_string(&request.callable_name)?;
    Ok(format!(
        r#"export interface WorkflowInput {{
  workingDirectory?: string;
  message?: string;
}}

export interface WorkflowOutput {{
  title: string;
  message: string;
}}

export const inputSchema = {{
  $schema: "https://json-schema.org/draft/2020-12/schema",
  type: "object",
  properties: {{
    workingDirectory: {{ type: "string", description: "Directory where the workflow was invoked." }},
    message: {{ type: "string", description: "Message to include in the result." }},
  }},
  additionalProperties: false,
}} as const;

export const outputSchema = {{
  $schema: "https://json-schema.org/draft/2020-12/schema",
  type: "object",
  properties: {{
    title: {{ type: "string" }},
    message: {{ type: "string" }},
  }},
  required: ["title", "message"],
  additionalProperties: false,
}} as const;

export interface CompletionRequest {{
  input: WorkflowInput;
  activeField?: string;
  prefix: string;
  mode: "field" | "value";
}}

export interface WorkflowContext {{
  progress(message: string, data?: unknown): void;
  requestUserInput?(request: unknown): Promise<unknown>;
}}

type WorkflowDefinition = {{
  apiVersion: 1;
  id: string;
  title: string;
  callableName: string;
  inputSchema: typeof inputSchema;
  outputSchema: typeof outputSchema;
  run(ctx: WorkflowContext, input: WorkflowInput): Promise<WorkflowOutput>;
  complete?(ctx: WorkflowContext, request: CompletionRequest): Promise<Array<{{ value: string; description?: string }}>>;
  format(output: WorkflowOutput, options: {{ format: "markdown.v1" }}): Promise<{{ markdown: string }}>;
}};

export function defineWorkflow(workflow: WorkflowDefinition): WorkflowDefinition {{
  return workflow;
}}

export default defineWorkflow({{
  apiVersion: 1,
  id: {id},
  title: {title},
  callableName: {callable_name},
  inputSchema,
  outputSchema,
  async run(ctx, input) {{
    ctx.progress("Running workflow");
    return {{ title: {title}, message: input.message ?? "Workflow completed." }};
  }},
  async complete(_ctx, _request) {{
    return [];
  }},
  async format(output, options) {{
    if (options.format !== "markdown.v1") throw new Error("Unsupported workflow format.");
    return {{ markdown: `# ${{output.title}}\n\n${{output.message}}\n` }};
  }},
}});
"#
    ))
}

fn workflow_test_source() -> &'static str {
    r#"// workflow-covers: positive load autocomplete negative
import { expect, test } from "bun:test";
import workflow, { inputSchema, outputSchema } from "../workflow";

test("loads the canonical workflow contract", () => {
  expect(workflow.apiVersion).toBe(1);
  expect(inputSchema.$schema).toBe("https://json-schema.org/draft/2020-12/schema");
  expect(outputSchema.$schema).toBe("https://json-schema.org/draft/2020-12/schema");
});

test("runs and formats markdown", async () => {
  const output = await workflow.run({ progress() {} }, { message: "Ready." });
  expect(await workflow.format(output, { format: "markdown.v1" })).toEqual({
    markdown: `# ${workflow.title}\n\nReady.\n`,
  });
});

test("provides bounded autocomplete results", async () => {
  expect(await workflow.complete?.(
    { progress() {} },
    { input: {}, prefix: "", mode: "field" },
  )).toEqual([]);
});

test("rejects unsupported formatter versions", async () => {
  await expect(workflow.format(
    { title: workflow.title, message: "Ready." },
    { format: "unsupported" as "markdown.v1" },
  )).rejects.toThrow("Unsupported workflow format");
});
"#
}

#[cfg(not(all(unix, not(target_os = "redox"))))]
fn initialize_git_repository(root: &Path) -> anyhow::Result<()> {
    let output = Command::new("git")
        .arg("init")
        .arg("--quiet")
        .arg(root)
        .output()
        .context("failed to start git while initializing workflow package")?;
    if !output.status.success() {
        bail!(
            "git init failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
#[path = "scaffold_tests.rs"]
mod tests;

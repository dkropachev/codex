#![allow(
    dead_code,
    reason = "wired into managed installation in the next slice"
)]

use std::fs;
use std::io::ErrorKind;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use anyhow::Context;
use anyhow::bail;
use codex_sandboxing::LocalSandboxRuntime;
use codex_sandboxing::LocalSandboxUnavailableReason;
use codex_utils_absolute_path::AbsolutePathBuf;

use super::ValidatedManagedDependencies;
use super::bun::ManagedBunCommandPlan;
use super::bun::ManagedBunInstallLockfile;
use super::bun::ManagedBunSandboxPreparation;
use super::lockfile::ManagedBunLockfile;

/// Inputs whose paths and dependency classification are revalidated before materialization.
pub(in crate::managed) struct ManagedDependencyMaterializationRequest<'a> {
    pub(in crate::managed) package: &'a crate::WorkflowPackage,
    pub(in crate::managed) dependencies: &'a ValidatedManagedDependencies,
    pub(in crate::managed) management_root: &'a AbsolutePathBuf,
    pub(in crate::managed) bun_executable: &'a AbsolutePathBuf,
    pub(in crate::managed) deadline: crate::runner::CommandDeadline,
    pub(in crate::managed) limits: crate::runner::CommandOutputLimits,
    pub(in crate::managed) cancelled: Option<&'a AtomicBool>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::managed) enum ManagedDependencyMaterializationOutcome {
    Materialized,
    SandboxUnavailable(LocalSandboxUnavailableReason),
}

enum ManagedBunExecution {
    Output(crate::runner::BoundedCommandOutput),
    SandboxUnavailable(LocalSandboxUnavailableReason),
}

/// Materializes text-lock dependencies exactly once in a mandatory sandbox.
///
/// This rereads package manifests and the lockfile, but intentionally does not establish candidate
/// identity. A successful result is not activation-ready: the next slice must enforce post-install
/// tree bounds and exact Git/index verification before publishing the candidate.
pub(in crate::managed) fn materialize_managed_dependencies(
    request: ManagedDependencyMaterializationRequest<'_>,
    runtime: LocalSandboxRuntime<'_>,
) -> anyhow::Result<ManagedDependencyMaterializationOutcome> {
    materialize_with_executor(request, |plan, deadline, limits, cancelled| {
        match plan.prepare(runtime)? {
            ManagedBunSandboxPreparation::Prepared(command) => Ok(ManagedBunExecution::Output(
                command.run(deadline, limits, cancelled)?,
            )),
            ManagedBunSandboxPreparation::Unavailable(reason) => {
                Ok(ManagedBunExecution::SandboxUnavailable(reason))
            }
        }
    })
}

fn materialize_with_executor(
    request: ManagedDependencyMaterializationRequest<'_>,
    execute: impl FnOnce(
        ManagedBunCommandPlan,
        crate::runner::CommandDeadline,
        crate::runner::CommandOutputLimits,
        Option<&AtomicBool>,
    ) -> anyhow::Result<ManagedBunExecution>,
) -> anyhow::Result<ManagedDependencyMaterializationOutcome> {
    request.deadline.check(request.cancelled)?;
    let fresh_package = crate::WorkflowPackage::load(&request.package.root)
        .context("failed to revalidate managed workflow before dependency installation")?;
    let fresh_dependencies = super::validate_managed_dependencies(&fresh_package)?;
    if fresh_dependencies != *request.dependencies {
        bail!("managed workflow dependency inputs changed after validation");
    }
    match fresh_dependencies.lockfile {
        ManagedBunLockfile::NotRequired => {
            return Ok(ManagedDependencyMaterializationOutcome::Materialized);
        }
        ManagedBunLockfile::BinaryRequiresSandboxInspection => {
            bail!("binary Bun lockfile inspection is not supported in this installation stage");
        }
        ManagedBunLockfile::TextSourcesValidated => {}
    }

    let candidate = AbsolutePathBuf::from_absolute_path_checked(&fresh_package.root)
        .context("managed workflow candidate must use an absolute path")?;
    reject_overlap(&candidate, request.management_root)?;
    let node_modules = candidate.join("node_modules");
    reject_existing_node_modules(&node_modules)?;
    request.deadline.check(request.cancelled)?;
    for writable_root in [&candidate, request.management_root] {
        if resolved_paths_overlap(request.bun_executable, writable_root)? {
            bail!("managed writable roots may not provide the Bun executable");
        }
    }

    let environment = super::bun::materialize_bun_environment(request.management_root)?;
    let plan = super::bun::managed_bun_install_command_plan(
        request.bun_executable,
        &candidate,
        ManagedBunInstallLockfile::Text,
        &fresh_dependencies.sources,
        &environment,
    )?;
    fs::create_dir(node_modules.as_path()).with_context(|| {
        format!(
            "failed to create managed dependency directory {}",
            node_modules.as_path().display()
        )
    })?;
    let mut cleanup = CreatedNodeModules::new(node_modules);
    let execution = match execute(plan, request.deadline, request.limits, request.cancelled) {
        Ok(execution) => execution,
        Err(error) => return Err(cleanup.with_original(error)),
    };
    match execution {
        ManagedBunExecution::SandboxUnavailable(reason) => {
            if let Err(cleanup_error) = cleanup.cleanup() {
                bail!(
                    "managed Bun sandbox was unavailable ({reason:?}); cleanup also failed: {cleanup_error:#}"
                );
            }
            Ok(ManagedDependencyMaterializationOutcome::SandboxUnavailable(
                reason,
            ))
        }
        ManagedBunExecution::Output(output) => {
            if let Err(error) = classify_output(&output) {
                return Err(cleanup.with_original(error));
            }
            cleanup.preserve();
            Ok(ManagedDependencyMaterializationOutcome::Materialized)
        }
    }
}

fn classify_output(output: &crate::runner::BoundedCommandOutput) -> anyhow::Result<()> {
    if output.stdout_oversized {
        bail!("managed Bun install stdout exceeded its configured limit");
    }
    if output.stderr_oversized {
        bail!("managed Bun install stderr exceeded its configured limit");
    }
    if !output.status.success() {
        bail!("managed Bun install failed with status {}", output.status);
    }
    Ok(())
}

fn reject_overlap(candidate: &AbsolutePathBuf, management: &AbsolutePathBuf) -> anyhow::Result<()> {
    if resolved_paths_overlap(candidate, management)? {
        bail!("managed workflow candidate and management root must not overlap");
    }
    Ok(())
}

fn resolved_paths_overlap(left: &AbsolutePathBuf, right: &AbsolutePathBuf) -> anyhow::Result<bool> {
    let left = resolve_existing_prefix(left.as_path())?;
    let right = resolve_existing_prefix(right.as_path())?;
    Ok(left.starts_with(&right) || right.starts_with(&left))
}

fn resolve_existing_prefix(path: &Path) -> anyhow::Result<PathBuf> {
    for ancestor in path.ancestors() {
        match fs::canonicalize(ancestor) {
            Ok(resolved) => return Ok(resolved.join(path.strip_prefix(ancestor)?)),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to resolve managed path {}", ancestor.display())
                });
            }
        }
    }
    bail!("managed path has no existing absolute ancestor")
}

pub(super) fn ensure_regular_directory_tree(
    management_root: &AbsolutePathBuf,
    path: &AbsolutePathBuf,
) -> anyhow::Result<()> {
    let anchor = management_root
        .parent()
        .context("managed directory root has no parent")?;
    let relative = path
        .as_path()
        .strip_prefix(anchor.as_path())
        .context("managed directory escaped its trusted parent")?;
    let mut current = anchor.as_path().to_path_buf();
    for component in relative.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(component) => {
                current.push(component);
                match fs::symlink_metadata(&current) {
                    Ok(_) => {}
                    Err(error) if error.kind() == ErrorKind::NotFound => {
                        fs::create_dir(&current).with_context(|| {
                            format!("failed to create managed directory {}", current.display())
                        })?;
                    }
                    Err(error) => {
                        return Err(error).with_context(|| {
                            format!("failed to inspect managed directory {}", current.display())
                        });
                    }
                }
                let metadata = fs::symlink_metadata(&current).with_context(|| {
                    format!("failed to inspect managed directory {}", current.display())
                })?;
                validate_directory_component(&current, &metadata)?;
            }
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => {
                bail!("managed directory contains an unsafe component");
            }
        }
    }
    Ok(())
}

fn validate_directory_component(path: &Path, metadata: &fs::Metadata) -> anyhow::Result<()> {
    if !metadata.is_dir() || metadata.file_type().is_symlink() || is_windows_reparse_point(metadata)
    {
        bail!(
            "managed path component {} must be a regular directory without aliases",
            path.display()
        );
    }
    Ok(())
}

fn reject_existing_node_modules(path: &AbsolutePathBuf) -> anyhow::Result<()> {
    match fs::symlink_metadata(path.as_path()) {
        Ok(_) => bail!("managed workflow candidate already contains node_modules"),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("failed to inspect managed workflow node_modules"),
    }
}

struct CreatedNodeModules {
    path: AbsolutePathBuf,
    armed: bool,
}

impl CreatedNodeModules {
    fn new(path: AbsolutePathBuf) -> Self {
        Self { path, armed: true }
    }

    fn cleanup(&mut self) -> anyhow::Result<()> {
        remove_created_tree(&self.path)?;
        self.armed = false;
        Ok(())
    }

    fn with_original(&mut self, original: anyhow::Error) -> anyhow::Error {
        match self.cleanup() {
            Ok(()) => original,
            Err(cleanup) => anyhow::anyhow!(
                "{original:#}; managed node_modules cleanup also failed: {cleanup:#}"
            ),
        }
    }

    fn preserve(mut self) {
        self.armed = false;
    }
}

impl Drop for CreatedNodeModules {
    fn drop(&mut self) {
        if self.armed {
            let _ = remove_created_tree(&self.path);
        }
    }
}

fn remove_created_tree(path: &AbsolutePathBuf) -> anyhow::Result<()> {
    let metadata = match fs::symlink_metadata(path.as_path()) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("failed to inspect managed node_modules cleanup"),
    };
    validate_directory_component(path.as_path(), &metadata)
        .context("refusing to follow replaced managed node_modules")?;
    fs::remove_dir_all(path.as_path()).context("failed to remove managed node_modules")
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

#[cfg(test)]
#[path = "install_tests.rs"]
mod tests;

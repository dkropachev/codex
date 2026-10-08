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
use super::bun::ManagedBunEnvironment;
use super::bun::ManagedBunInstallLockfile;
use super::bun::ManagedBunSandboxPreparation;
use super::bun::PreparedManagedBunCommand;
use super::lockfile::ManagedBunLockfile;

mod binary_lock;
mod file;

use file::validate_directory_component;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ManagedBunPhase {
    Install,
    InspectBinaryLockfile,
}

enum ManagedBunPreparation<Prepared> {
    Prepared(Prepared),
    SandboxUnavailable(LocalSandboxUnavailableReason),
}

/// Separates sandbox preparation from bounded Bun execution.
///
/// Production implementations prepare commands through the local sandbox runtime. Tests use a
/// private fake to assert that each accepted plan is prepared before its prepared value is consumed
/// by exactly one `run` call. Implementations must return sandbox unavailability as a typed
/// preparation outcome; execution failures remain errors.
trait ManagedBunExecutor {
    type Prepared;

    /// Prepares `plan` for `phase` without executing it.
    fn prepare(
        &mut self,
        phase: ManagedBunPhase,
        plan: ManagedBunCommandPlan,
    ) -> anyhow::Result<ManagedBunPreparation<Self::Prepared>>;

    /// Consumes one successfully prepared command and runs it with the supplied bounds.
    fn run(
        &mut self,
        phase: ManagedBunPhase,
        prepared: Self::Prepared,
        environment: &ManagedBunEnvironment,
        deadline: crate::runner::CommandDeadline,
        limits: crate::runner::CommandOutputLimits,
        cancelled: Option<&AtomicBool>,
    ) -> anyhow::Result<crate::runner::BoundedCommandOutput>;
}

struct LocalManagedBunExecutor<'a, 'runtime> {
    runtime: &'a LocalSandboxRuntime<'runtime>,
}

impl ManagedBunExecutor for LocalManagedBunExecutor<'_, '_> {
    type Prepared = PreparedManagedBunCommand;

    fn prepare(
        &mut self,
        _phase: ManagedBunPhase,
        plan: ManagedBunCommandPlan,
    ) -> anyhow::Result<ManagedBunPreparation<Self::Prepared>> {
        Ok(match plan.prepare(self.runtime)? {
            ManagedBunSandboxPreparation::Prepared(command) => {
                ManagedBunPreparation::Prepared(command)
            }
            ManagedBunSandboxPreparation::Unavailable(reason) => {
                ManagedBunPreparation::SandboxUnavailable(reason)
            }
        })
    }

    fn run(
        &mut self,
        _phase: ManagedBunPhase,
        prepared: Self::Prepared,
        _environment: &ManagedBunEnvironment,
        deadline: crate::runner::CommandDeadline,
        limits: crate::runner::CommandOutputLimits,
        cancelled: Option<&AtomicBool>,
    ) -> anyhow::Result<crate::runner::BoundedCommandOutput> {
        prepared.run(deadline, limits, cancelled)
    }
}

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

/// Materializes validated Bun dependencies exactly once in a mandatory sandbox.
///
/// This rereads package manifests and the lockfile, but intentionally does not establish candidate
/// identity. A successful result is not activation-ready: the next slice must enforce post-install
/// tree bounds and exact Git/index verification before publishing the candidate.
pub(in crate::managed) fn materialize_managed_dependencies(
    request: ManagedDependencyMaterializationRequest<'_>,
    runtime: LocalSandboxRuntime<'_>,
) -> anyhow::Result<ManagedDependencyMaterializationOutcome> {
    materialize_with_executor(request, &mut LocalManagedBunExecutor { runtime: &runtime })
}

fn materialize_with_executor<E: ManagedBunExecutor>(
    request: ManagedDependencyMaterializationRequest<'_>,
    executor: &mut E,
) -> anyhow::Result<ManagedDependencyMaterializationOutcome> {
    request.deadline.check(request.cancelled)?;
    let fresh_package = crate::WorkflowPackage::load(&request.package.root)
        .context("failed to revalidate managed workflow before dependency installation")?;
    let fresh_dependencies = super::validate_managed_dependencies(&fresh_package)?;
    if fresh_dependencies != *request.dependencies {
        bail!("managed workflow dependency inputs changed after validation");
    }
    let lockfile = match fresh_dependencies.lockfile {
        ManagedBunLockfile::NotRequired => {
            return Ok(ManagedDependencyMaterializationOutcome::Materialized);
        }
        ManagedBunLockfile::BinaryRequiresSandboxInspection => ManagedBunInstallLockfile::Binary,
        ManagedBunLockfile::TextSourcesValidated => ManagedBunInstallLockfile::Text,
    };

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
    let staged = if lockfile == ManagedBunInstallLockfile::Binary {
        Some(binary_lock::stage_binary_lock_inputs(
            &candidate,
            &environment,
        )?)
    } else {
        None
    };
    let plan = super::bun::managed_bun_install_command_plan(
        request.bun_executable,
        &candidate,
        lockfile,
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
    let prepared = match executor.prepare(ManagedBunPhase::Install, plan) {
        Ok(ManagedBunPreparation::Prepared(prepared)) => prepared,
        Ok(ManagedBunPreparation::SandboxUnavailable(reason)) => {
            if let Err(cleanup_error) = cleanup.cleanup() {
                bail!(
                    "managed Bun sandbox was unavailable ({reason:?}); cleanup also failed: {cleanup_error:#}"
                );
            }
            return Ok(ManagedDependencyMaterializationOutcome::SandboxUnavailable(
                reason,
            ));
        }
        Err(error) => return Err(cleanup.with_original(error)),
    };
    if let Some(staged) = staged {
        let inspection_plan = match super::bun::managed_bun_binary_inspection_command_plan(
            request.bun_executable,
            &candidate,
            &environment,
        ) {
            Ok(plan) => plan,
            Err(error) => return Err(cleanup.with_original(error)),
        };
        let inspected = match executor
            .prepare(ManagedBunPhase::InspectBinaryLockfile, inspection_plan)
        {
            Ok(ManagedBunPreparation::Prepared(prepared)) => prepared,
            Ok(ManagedBunPreparation::SandboxUnavailable(reason)) => {
                if let Err(cleanup_error) = cleanup.cleanup() {
                    bail!(
                        "managed Bun inspection sandbox was unavailable ({reason:?}); cleanup also failed: {cleanup_error:#}"
                    );
                }
                return Ok(ManagedDependencyMaterializationOutcome::SandboxUnavailable(
                    reason,
                ));
            }
            Err(error) => return Err(cleanup.with_original(error)),
        };
        let output = match executor.run(
            ManagedBunPhase::InspectBinaryLockfile,
            inspected,
            &environment,
            request.deadline,
            request.limits,
            request.cancelled,
        ) {
            Ok(output) => output,
            Err(error) => return Err(cleanup.with_original(error)),
        };
        if let Err(error) = classify_output(ManagedBunPhase::InspectBinaryLockfile, &output) {
            return Err(cleanup.with_original(error));
        }
        if let Err(error) = request.deadline.check(request.cancelled).and_then(|()| {
            binary_lock::validate_binary_lock_inspection(
                &staged,
                &environment,
                &fresh_package,
                &fresh_dependencies.sources,
            )
        }) {
            return Err(cleanup.with_original(error));
        }
    }
    if let Err(error) = request.deadline.check(request.cancelled) {
        return Err(cleanup.with_original(error));
    }
    let output = match executor.run(
        ManagedBunPhase::Install,
        prepared,
        &environment,
        request.deadline,
        request.limits,
        request.cancelled,
    ) {
        Ok(output) => output,
        Err(error) => return Err(cleanup.with_original(error)),
    };
    if let Err(error) = classify_output(ManagedBunPhase::Install, &output) {
        return Err(cleanup.with_original(error));
    }
    cleanup.preserve();
    Ok(ManagedDependencyMaterializationOutcome::Materialized)
}

fn classify_output(
    phase: ManagedBunPhase,
    output: &crate::runner::BoundedCommandOutput,
) -> anyhow::Result<()> {
    let operation = match phase {
        ManagedBunPhase::Install => "install",
        ManagedBunPhase::InspectBinaryLockfile => "inspection",
    };
    if output.stdout_oversized {
        bail!("managed Bun {operation} stdout exceeded its configured limit");
    }
    if output.stderr_oversized {
        bail!("managed Bun {operation} stderr exceeded its configured limit");
    }
    if !output.status.success() {
        #[cfg(test)]
        eprintln!(
            "managed Bun {operation} stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        bail!(
            "managed Bun {operation} failed with status {}",
            output.status
        );
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

#[cfg(test)]
#[path = "install_tests.rs"]
mod tests;

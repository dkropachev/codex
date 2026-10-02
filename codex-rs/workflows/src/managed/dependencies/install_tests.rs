use std::cell::Cell;
use std::fs;
use std::path::Path;
use std::process::ExitStatus;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use codex_sandboxing::LocalSandboxUnavailableReason;
use pretty_assertions::assert_eq;

use super::*;

const MANIFEST: &str = "apiVersion: 1\nid: test/workflow\ntitle: Test workflow\ncallableName: test-workflow\ndescription: Test package\nvalidation:\n  commands: []\n  coverage:\n    positive: true\n    load: true\n    autocomplete: true\n    negative: true\n";

#[derive(Clone, Copy)]
enum Lock {
    None,
    Text,
    Binary,
}

struct Fixture {
    _temporary: tempfile::TempDir,
    package: crate::WorkflowPackage,
    dependencies: ValidatedManagedDependencies,
    management: AbsolutePathBuf,
    bun: AbsolutePathBuf,
}

impl Fixture {
    fn new(lock: Lock) -> Self {
        let temporary = tempfile::tempdir().expect("temporary fixture");
        let candidate = temporary.path().join("candidate");
        fs::create_dir_all(candidate.join("src")).expect("create source directory");
        fs::create_dir(temporary.path().join("tools")).expect("create tools directory");
        fs::write(temporary.path().join("tools/bun"), "bun").expect("write Bun executable");
        fs::write(candidate.join("workflow.yaml"), MANIFEST).expect("write manifest");
        let package_json = match lock {
            Lock::None => serde_json::json!({}),
            Lock::Text | Lock::Binary => {
                serde_json::json!({"dependencies": {"dep": "1.0.0"}})
            }
        };
        fs::write(candidate.join("package.json"), package_json.to_string())
            .expect("write package.json");
        fs::write(candidate.join("src/workflow.ts"), "export default {};\n")
            .expect("write workflow source");
        match lock {
            Lock::None => {}
            Lock::Binary => fs::write(candidate.join("bun.lockb"), [0]).expect("write bun.lockb"),
            Lock::Text => fs::write(
                candidate.join("bun.lock"),
                r#"{"lockfileVersion":1,"workspaces":{"":{"dependencies":{"dep":"1.0.0"}}},"packages":{"dep":["dep@1.0.0","",{},"integrity"]}}"#,
            )
            .expect("write bun.lock"),
        }
        let package = crate::WorkflowPackage::load(&candidate).expect("load package");
        let dependencies =
            super::super::validate_managed_dependencies(&package).expect("validate dependencies");
        Self {
            management: absolute(&temporary.path().join("management")),
            bun: absolute(&temporary.path().join("tools/bun")),
            _temporary: temporary,
            package,
            dependencies,
        }
    }

    fn request<'a>(
        &'a self,
        management_root: &'a AbsolutePathBuf,
        cancelled: Option<&'a AtomicBool>,
    ) -> ManagedDependencyMaterializationRequest<'a> {
        ManagedDependencyMaterializationRequest {
            package: &self.package,
            dependencies: &self.dependencies,
            management_root,
            bun_executable: &self.bun,
            deadline: crate::runner::CommandDeadline::after(Duration::from_secs(/*secs*/ 2)),
            limits: crate::runner::CommandOutputLimits {
                stdout_bytes: 32,
                stderr_bytes: 32,
            },
            cancelled,
        }
    }

    fn normal_request(&self) -> ManagedDependencyMaterializationRequest<'_> {
        self.request(&self.management, /*cancelled*/ None)
    }

    fn node_modules(&self) -> std::path::PathBuf {
        self.package.root.join("node_modules")
    }
}

fn absolute(path: &Path) -> AbsolutePathBuf {
    AbsolutePathBuf::from_absolute_path_checked(path).expect("absolute fixture path")
}

fn output(code: i32, stdout_oversized: bool, stderr_oversized: bool) -> ManagedBunExecution {
    ManagedBunExecution::Output(crate::runner::BoundedCommandOutput {
        status: exit_status(code),
        stdout: b"stdout-secret".to_vec(),
        stderr: b"stderr-secret".to_vec(),
        stdout_oversized,
        stderr_oversized,
    })
}

#[cfg(unix)]
fn exit_status(code: i32) -> ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    ExitStatus::from_raw(code << 8)
}

#[cfg(windows)]
fn exit_status(code: i32) -> ExitStatus {
    use std::os::windows::process::ExitStatusExt;
    ExitStatus::from_raw(code as u32)
}

#[test]
fn dependency_classification_controls_zero_mutation_and_exactly_one_install() {
    for lock in [Lock::None, Lock::Binary] {
        let fixture = Fixture::new(lock);
        let result = materialize_with_executor(fixture.normal_request(), |_, _, _, _| {
            panic!("non-text package executed Bun")
        });
        match lock {
            Lock::None => assert_eq!(
                result.expect("dependency-free package"),
                ManagedDependencyMaterializationOutcome::Materialized
            ),
            Lock::Binary => assert!(
                result
                    .expect_err("defer binary lock")
                    .to_string()
                    .contains("binary Bun lockfile inspection")
            ),
            Lock::Text => unreachable!(),
        }
        assert!(!fixture.management.as_path().exists());
        assert!(!fixture.node_modules().exists());
    }

    let fixture = Fixture::new(Lock::Text);
    let calls = Cell::new(0);
    let outcome = materialize_with_executor(fixture.normal_request(), |plan, _, limits, cancel| {
        calls.set(calls.get() + 1);
        drop(plan);
        assert_eq!(limits.stdout_bytes, 32);
        assert!(cancel.is_none());
        assert!(fixture.node_modules().is_dir());
        Ok(output(
            /*code*/ 0, /*stdout_oversized*/ false, /*stderr_oversized*/ false,
        ))
    })
    .expect("install text lock");
    assert_eq!(
        outcome,
        ManagedDependencyMaterializationOutcome::Materialized
    );
    assert_eq!(calls.get(), 1);
    assert!(fixture.node_modules().is_dir());

    let cancelled = AtomicBool::new(true);
    for lock in [Lock::None, Lock::Binary] {
        let fixture = Fixture::new(lock);
        let error = materialize_with_executor(
            fixture.request(&fixture.management, Some(&cancelled)),
            |_, _, _, _| panic!("pre-cancelled package executed Bun"),
        )
        .expect_err("cancel before dependency classification");
        assert!(error.to_string().contains("cancelled"));
        assert!(!fixture.management.as_path().exists());
        assert!(!fixture.node_modules().exists());
    }
}

#[test]
fn unavailable_errors_cancellation_and_partial_output_clean_up() {
    let fixture = Fixture::new(Lock::Text);
    assert_eq!(
        materialize_with_executor(fixture.normal_request(), |plan, _, _, _| {
            drop(plan);
            Ok(ManagedBunExecution::SandboxUnavailable(
                LocalSandboxUnavailableReason::SelectionUnavailable,
            ))
        })
        .expect("typed sandbox result"),
        ManagedDependencyMaterializationOutcome::SandboxUnavailable(
            LocalSandboxUnavailableReason::SelectionUnavailable
        )
    );
    assert!(!fixture.node_modules().exists());

    let failed = Fixture::new(Lock::Text);
    let node_modules = failed.node_modules();
    let error = materialize_with_executor(failed.normal_request(), |plan, _, _, _| {
        drop(plan);
        fs::create_dir_all(node_modules.join("partial/tree")).expect("write partial tree");
        Err(anyhow::anyhow!("sandbox preparation failed"))
    })
    .expect_err("reject failed preparation");
    assert!(error.to_string().contains("sandbox preparation failed"));
    assert!(!node_modules.exists());

    let running = Fixture::new(Lock::Text);
    let cancelled = AtomicBool::new(false);
    let result = materialize_with_executor(
        running.request(&running.management, Some(&cancelled)),
        |plan, deadline, _, signal| {
            drop(plan);
            cancelled.store(true, Ordering::Relaxed);
            deadline.check(signal)?;
            unreachable!()
        },
    );
    assert!(
        result
            .expect_err("cancel running install")
            .to_string()
            .contains("cancelled")
    );
    assert!(!running.node_modules().exists());

    let fixture = Fixture::new(Lock::Text);
    let cancelled = AtomicBool::new(true);
    let result = materialize_with_executor(
        fixture.request(&fixture.management, Some(&cancelled)),
        |_, _, _, _| panic!("pre-cancelled install executed"),
    );
    assert!(
        result
            .expect_err("cancel install")
            .to_string()
            .contains("cancelled")
    );
    assert!(!fixture.management.as_path().exists());
}

#[test]
fn bounded_output_failures_use_the_production_classifier_without_secrets() {
    for (code, stdout, stderr, expected) in [
        (1, false, false, "failed with status"),
        (0, true, false, "stdout exceeded"),
        (0, false, true, "stderr exceeded"),
    ] {
        let fixture = Fixture::new(Lock::Text);
        let error = materialize_with_executor(fixture.normal_request(), |plan, _, _, _| {
            drop(plan);
            Ok(output(
                code, /*stdout_oversized*/ stdout, /*stderr_oversized*/ stderr,
            ))
        })
        .expect_err("reject bounded output");
        let message = format!("{error:#}");
        assert!(message.contains(expected));
        assert!(!message.contains("stdout-secret") && !message.contains("stderr-secret"));
        assert!(!fixture.node_modules().exists());
    }
}

#[test]
fn stale_existing_and_overlapping_candidates_fail_before_mutation() {
    let existing = Fixture::new(Lock::Text);
    fs::create_dir(existing.node_modules()).expect("create existing node_modules");
    assert!(
        materialize_with_executor(existing.normal_request(), |_, _, _, _| unreachable!()).is_err()
    );
    assert!(!existing.management.as_path().exists());

    let special = Fixture::new(Lock::Text);
    fs::write(special.management.as_path(), "not a directory").expect("write special root");
    assert!(
        materialize_with_executor(special.normal_request(), |_, _, _, _| unreachable!()).is_err()
    );
    assert!(!special.management.join("bun").as_path().exists());

    for suffix in ["", "node_modules/management"] {
        let fixture = Fixture::new(Lock::Text);
        let management = absolute(&fixture.package.root.join(suffix));
        assert!(
            materialize_with_executor(
                fixture.request(&management, /*cancelled*/ None),
                |_, _, _, _| unreachable!()
            )
            .is_err()
        );
        assert!(!fixture.node_modules().exists());
        assert!(!fixture.package.root.join("bun").exists());
    }

    let changed = Fixture::new(Lock::Text);
    fs::write(
        changed.package.root.join("package.json"),
        r#"{"dependencies":{"other":"1"}}"#,
    )
    .expect("change package.json");
    assert!(
        materialize_with_executor(changed.normal_request(), |_, _, _, _| unreachable!()).is_err()
    );
    assert!(!changed.management.as_path().exists());
}

#[cfg(unix)]
#[test]
fn aliases_are_rejected_and_replaced_cleanup_roots_are_reported() {
    use std::os::unix::fs::symlink;

    let aliased = Fixture::new(Lock::Text);
    let parent = aliased.management.parent().expect("fixture root");
    let real = parent.join("real-management");
    fs::create_dir(real.as_path()).expect("create alias target");
    symlink(real.as_path(), aliased.management.as_path()).expect("alias management root");
    assert!(
        materialize_with_executor(
            aliased.request(&aliased.management, /*cancelled*/ None),
            |_, _, _, _| unreachable!()
        )
        .is_err()
    );
    assert!(!real.join("bun").as_path().exists());

    let replaced = Fixture::new(Lock::Text);
    let node_modules = replaced.node_modules();
    let external = replaced
        .management
        .parent()
        .expect("fixture root")
        .join("external");
    fs::create_dir(external.as_path()).expect("create external tree");
    fs::write(external.join("keep").as_path(), "keep").expect("write marker");
    let error = materialize_with_executor(replaced.normal_request(), |plan, _, _, _| {
        drop(plan);
        fs::remove_dir(&node_modules).expect("remove node_modules");
        symlink(external.as_path(), &node_modules).expect("replace with alias");
        Err(anyhow::anyhow!("install failed"))
    })
    .expect_err("surface cleanup failure");
    let message = format!("{error:#}");
    assert!(message.contains("install failed") && message.contains("cleanup also failed"));
    assert!(external.join("keep").as_path().is_file());
    assert!(
        fs::symlink_metadata(node_modules)
            .expect("retained alias")
            .file_type()
            .is_symlink()
    );
}

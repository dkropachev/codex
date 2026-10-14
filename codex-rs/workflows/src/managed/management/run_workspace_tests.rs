#![allow(clippy::expect_used)]

use std::fs;
use std::fs::TryLockError;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;

use super::*;
use crate::managed::management::ManagedWorkflowInstallRequest;

const MANIFEST: &str = "apiVersion: 1\nid: team/build\ntitle: Team Build\ncallableName: team-build\ndescription: Build workflow\nvalidation:\n  commands: []\n  coverage:\n    positive: true\n    load: true\n    autocomplete: true\n    negative: true\n";

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .args(args)
        .output()
        .expect("run Git");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn run_workspace_is_a_verified_disposable_copy() {
    let root = tempfile::tempdir().expect("root");
    let source = root.path().join("source");
    fs::create_dir(&source).expect("source directory");
    fs::create_dir(source.join("src")).expect("source subtree");
    fs::write(source.join("workflow.yaml"), MANIFEST).expect("manifest");
    fs::write(source.join("package.json"), "{}").expect("package");
    fs::write(source.join("src/workflow.ts"), "export default {};\n").expect("source code");
    git(&source, &["init", "-q"]);
    git(&source, &["add", "--all"]);
    git(
        &source,
        &["-c", "commit.gpgsign=false", "commit", "-qm", "initial"],
    );
    let home = AbsolutePathBuf::from_absolute_path_checked(root.path()).expect("home");
    let workflows = home.join("workflows");
    let service = ManagedWorkflowService::new(&home, &workflows).expect("service");
    let cancelled = AtomicBool::new(false);
    service
        .install(ManagedWorkflowInstallRequest {
            source: source.to_str().expect("source path"),
            dependency_runtime: None,
            cancelled: &cancelled,
        })
        .expect("managed install");
    let active = workflows.join("team/build");
    let installed = service.list_installed().expect("records")[0]
        .installed
        .clone();
    assert!(
        service
            .prepare_run_workspace(
                "team/build",
                workflows.join("zz-override").as_path(),
                &cancelled
            )
            .is_err()
    );
    let other = ManagedWorkflowService::new(&home, &workflows).expect("second service");
    let workspace = service
        .prepare_run_workspace("team/build", active.as_path(), &cancelled)
        .expect("run workspace");
    let lock_file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(home.join(".workflow-management/locks/team/build.lock"))
        .expect("workflow lock file");
    assert!(matches!(
        lock_file.try_lock(),
        Err(TryLockError::WouldBlock)
    ));
    let (started_tx, started_rx) = mpsc::channel();
    let (acquired_tx, acquired_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        started_tx.send(()).expect("worker started");
        let _locked = other
            .store
            .lock_install("team/build", /*cancelled*/ None)
            .expect("exclusive workflow lock");
        acquired_tx.send(()).expect("worker acquired lock");
    });
    started_rx.recv().expect("worker start signal");
    assert!(
        acquired_rx
            .recv_timeout(Duration::from_millis(/*millis*/ 100))
            .is_err()
    );
    assert_eq!(workspace.id(), "team/build");
    assert_ne!(workspace.root(), active.as_path());
    assert_eq!(
        fs::read_to_string(workspace.root().join("src/workflow.ts")).expect("copied source"),
        "export default {};\n"
    );
    fs::create_dir(workspace.root().join("state")).expect("runtime state directory");
    fs::create_dir(workspace.root().join("artifacts")).expect("runtime artifact directory");
    fs::write(workspace.root().join("state/session.json"), "{}\n").expect("runtime state");
    fs::write(workspace.root().join("artifacts/report.md"), "report\n").expect("runtime artifact");
    assert!(!active.join("state/session.json").exists());
    assert!(!active.join("artifacts/report.md").exists());
    let workspace_root = workspace.root().to_path_buf();
    drop(workspace);
    acquired_rx
        .recv_timeout(Duration::from_secs(/*secs*/ 5))
        .expect("exclusive lock after run");
    worker.join().expect("worker joined");
    lock_file.try_lock().expect("lock released after run");
    drop(lock_file);
    assert!(!workspace_root.exists());
    fs::write(active.join("src/workflow.ts"), "modified locally\n").expect("dirty source");
    assert!(
        service
            .prepare_run_workspace("team/build", active.as_path(), &cancelled)
            .is_err()
    );
    fs::write(active.join("src/workflow.ts"), "export default {};\n").expect("restore source");
    service
        .uninstall("team/build", &installed, &cancelled)
        .expect("clean install can be removed after runtime writes");
}

use std::fs;
use std::path::Path;
use std::time::Duration;

use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;

use super::super::fs::SecureDirectory;
use super::*;

#[test]
fn windows_copy_preserves_payload_and_excludes_checkout_git() {
    let root = tempfile::tempdir().expect("root");
    let source = root.path().join("source");
    fs::create_dir_all(source.join("src")).expect("source directory");
    fs::create_dir(source.join(".git")).expect("checkout metadata");
    fs::write(source.join("workflow.yaml"), b"workflow").expect("manifest");
    fs::write(source.join("src/workflow.ts"), b"export default {};").expect("source file");
    let source = AbsolutePathBuf::from_absolute_path_checked(&source).expect("source path");
    let root_path = AbsolutePathBuf::from_absolute_path_checked(root.path()).expect("root path");
    let destination = SecureDirectory::open_root(&root_path)
        .expect("open root")
        .child("managed")
        .expect("private managed directory");
    copy_verified_payload(
        &source,
        &destination,
        crate::managed::fetch::VERIFICATION_LIMITS,
        crate::runner::CommandDeadline::after(Duration::from_secs(/*secs*/ 5)),
        /*cancelled*/ None,
    )
    .expect("copy payload");
    assert_eq!(
        fs::read(destination.path().join("payload/src/workflow.ts").as_path())
            .expect("copied source"),
        b"export default {};"
    );
    assert!(!destination.path().join("payload/.git").exists());
}

#[test]
fn windows_copy_enforces_logical_byte_limit() {
    let root = tempfile::tempdir().expect("root");
    let source = root.path().join("source");
    fs::create_dir(&source).expect("source directory");
    fs::write(source.join("file"), b"five!").expect("source file");
    let source = AbsolutePathBuf::from_absolute_path_checked(&source).expect("source path");
    let root_path = AbsolutePathBuf::from_absolute_path_checked(root.path()).expect("root path");
    let destination = SecureDirectory::open_root(&root_path)
        .expect("open root")
        .child("managed")
        .expect("private managed directory");
    let mut limits = crate::managed::fetch::VERIFICATION_LIMITS;
    limits.post_install_bytes = 4;
    assert!(
        copy_verified_payload(
            &source,
            &destination,
            limits,
            crate::runner::CommandDeadline::after(Duration::from_secs(/*secs*/ 5)),
            /*cancelled*/ None,
        )
        .is_err()
    );
}

#[test]
fn windows_copy_rejects_junction_entries() {
    let root = tempfile::tempdir().expect("root");
    let source = root.path().join("source");
    let modules = source.join("node_modules");
    fs::create_dir_all(&modules).expect("dependency directory");
    let outside = root.path().join("outside");
    fs::create_dir(&outside).expect("outside directory");
    fs::write(outside.join("secret"), b"outside").expect("outside file");
    let junction = modules.join("junction");
    let output = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$ErrorActionPreference = 'Stop'; New-Item -ItemType Junction -Path $env:CODEX_TEST_LINK -Target $env:CODEX_TEST_TARGET | Out-Null",
        ])
        .env("CODEX_TEST_LINK", &junction)
        .env("CODEX_TEST_TARGET", &outside)
        .output()
        .expect("create junction");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let source = AbsolutePathBuf::from_absolute_path_checked(&source).expect("source path");
    let root_path = AbsolutePathBuf::from_absolute_path_checked(root.path()).expect("root path");
    let destination = SecureDirectory::open_root(&root_path)
        .expect("open root")
        .child("managed")
        .expect("private managed directory");
    assert!(
        copy_verified_payload(
            &source,
            &destination,
            crate::managed::fetch::VERIFICATION_LIMITS,
            crate::runner::CommandDeadline::after(Duration::from_secs(/*secs*/ 5)),
            /*cancelled*/ None,
        )
        .is_err()
    );
}

#[test]
fn windows_copy_rejects_nested_links_through_a_junction() {
    let root = tempfile::tempdir().expect("root");
    let source = root.path().join("source");
    let modules = source.join("node_modules");
    fs::create_dir_all(modules.join("-aliasdir")).expect("alias parent");
    fs::create_dir(modules.join(".bin")).expect("dependency bin");
    let outside = root.path().join("outside");
    fs::create_dir(&outside).expect("outside directory");
    fs::write(outside.join("secret"), b"outside").expect("outside file");
    let junction = modules.join("-aliasdir/junction");
    let output = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$ErrorActionPreference = 'Stop'; New-Item -ItemType Junction -Path $env:CODEX_TEST_LINK -Target $env:CODEX_TEST_TARGET | Out-Null",
        ])
        .env("CODEX_TEST_LINK", &junction)
        .env("CODEX_TEST_TARGET", &outside)
        .output()
        .expect("create nested junction");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let link = modules.join(".bin/tool");
    let target = Path::new(r"..\-aliasdir\junction\secret");
    if std::os::windows::fs::symlink_file(target, &link).is_err() {
        return; // Symlink creation requires Developer Mode or the relevant Windows privilege.
    }
    let source = AbsolutePathBuf::from_absolute_path_checked(&source).expect("source path");
    let root_path = AbsolutePathBuf::from_absolute_path_checked(root.path()).expect("root path");
    let destination = SecureDirectory::open_root(&root_path)
        .expect("open root")
        .child("managed")
        .expect("private managed directory");
    let error = copy_verified_payload(
        &source,
        &destination,
        crate::managed::fetch::VERIFICATION_LIMITS,
        crate::runner::CommandDeadline::after(Duration::from_secs(/*secs*/ 5)),
        /*cancelled*/ None,
    )
    .expect_err("nested junction must be rejected");
    assert!(
        error
            .to_string()
            .contains("dependency link resolves through a reparse point"),
        "{error:#}"
    );
}

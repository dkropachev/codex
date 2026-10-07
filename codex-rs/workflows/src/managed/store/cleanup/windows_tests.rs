use std::fs;

use codex_utils_absolute_path::AbsolutePathBuf;

use super::*;

fn staging(root: &std::path::Path) -> SecureDirectory {
    let absolute = AbsolutePathBuf::from_absolute_path_checked(root).expect("absolute root");
    SecureDirectory::open_root(&absolute)
        .expect("open root")
        .child("staging")
        .expect("private staging")
}

fn marked(parent: &SecureDirectory, name: &str) -> SecureDirectory {
    parent
        .write_file(
            &ownership_record_name(name),
            &reservation_record(name),
            /*replace*/ false,
        )
        .expect("reserve name");
    let owned = parent.child(name).expect("owned root");
    let (device, inode) = owned.identity().expect("identity");
    parent
        .write_file(
            &ownership_record_name(name),
            &bound_record(name, device, inode),
            /*replace*/ true,
        )
        .expect("bind record");
    owned
        .write_file(
            ".codex-managed-operation",
            name.as_bytes(),
            /*replace*/ false,
        )
        .expect("operation marker");
    owned
}

#[test]
fn removes_only_marked_tree_and_preserves_external_files() {
    let root = tempfile::tempdir().expect("root");
    let external = tempfile::tempdir().expect("external");
    fs::write(external.path().join("keep"), b"keep").expect("external file");
    let parent = staging(root.path());
    let owned = marked(&parent, "tx-owned");
    owned
        .child("payload")
        .expect("payload")
        .write_file("file", b"content", /*replace*/ false)
        .expect("payload file");
    let (device, inode) = owned.identity().expect("identity");
    drop(owned);
    remove_tree(
        &parent,
        "tx-owned",
        device,
        inode,
        OwnershipMarker::Required,
        CleanupEntryLimit::STANDARD,
    )
    .expect("remove owned tree");
    assert!(!parent.path().join("tx-owned").exists());
    assert!(
        !parent
            .path()
            .join(ownership_record_name("tx-owned"))
            .exists()
    );
    assert_eq!(
        fs::read(external.path().join("keep")).expect("external file"),
        b"keep"
    );
}

#[test]
fn stale_bound_record_cannot_delete_replacement_directory() {
    let root = tempfile::tempdir().expect("root");
    let parent = staging(root.path());
    let original = marked(&parent, "tx-old");
    let old_path = original.path().as_path().to_path_buf();
    drop(original);
    let moved = root.path().join("moved");
    fs::rename(&old_path, &moved).expect("move owned root");
    let replacement = parent.child("tx-old").expect("replacement root");
    let (device, inode) = replacement.identity().expect("replacement identity");
    drop(replacement);
    assert!(
        remove_tree(
            &parent,
            "tx-old",
            device,
            inode,
            OwnershipMarker::Required,
            CleanupEntryLimit::STANDARD,
        )
        .is_err()
    );
    assert!(old_path.is_dir() && moved.is_dir());
}

#[test]
fn directory_symlink_cleanup_preserves_external_tree() {
    let root = tempfile::tempdir().expect("root");
    let external = tempfile::tempdir().expect("external");
    fs::write(external.path().join("keep"), b"keep").expect("external file");
    let parent = staging(root.path());
    let owned = marked(&parent, "tx-link");
    if std::os::windows::fs::symlink_dir(external.path(), owned.path().join("alias").as_path())
        .is_err()
    {
        return; // Creating symlinks requires Developer Mode or the Windows privilege.
    }
    let (device, inode) = owned.identity().expect("identity");
    drop(owned);
    remove_tree(
        &parent,
        "tx-link",
        device,
        inode,
        OwnershipMarker::Required,
        CleanupEntryLimit::STANDARD,
    )
    .expect("unlink owned directory symlink");
    assert!(!parent.path().join("tx-link").exists());
    assert_eq!(
        fs::read(external.path().join("keep")).expect("external file"),
        b"keep"
    );
}

#[test]
fn unknown_junction_preserves_marked_root_and_external_target() {
    let root = tempfile::tempdir().expect("root");
    let external = tempfile::tempdir().expect("external");
    fs::write(external.path().join("keep"), b"keep").expect("external file");
    let parent = staging(root.path());
    let owned = marked(&parent, "tx-junction");
    let junction = owned.path().join("junction");
    let output = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$ErrorActionPreference = 'Stop'; New-Item -ItemType Junction -Path $env:CODEX_TEST_LINK -Target $env:CODEX_TEST_TARGET | Out-Null",
        ])
        .env("CODEX_TEST_LINK", junction.as_path())
        .env("CODEX_TEST_TARGET", external.path())
        .output()
        .expect("create junction");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (device, inode) = owned.identity().expect("identity");
    drop(owned);
    assert!(
        remove_tree(
            &parent,
            "tx-junction",
            device,
            inode,
            OwnershipMarker::Required,
            CleanupEntryLimit::STANDARD,
        )
        .is_err()
    );
    assert!(parent.path().join("tx-junction").is_dir());
    assert!(
        parent
            .path()
            .join(ownership_record_name("tx-junction"))
            .is_file()
    );
    assert_eq!(
        fs::read(external.path().join("keep")).expect("external file"),
        b"keep"
    );
}

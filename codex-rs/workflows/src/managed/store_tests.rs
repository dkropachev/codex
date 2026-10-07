use std::fs;
use std::path::Path;

use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;

use super::fs::SecureDirectory;

fn absolute(path: &Path) -> AbsolutePathBuf {
    AbsolutePathBuf::from_absolute_path_checked(path).expect("absolute fixture path")
}

#[test]
fn private_directories_and_atomic_files_are_reusable() {
    let root = tempfile::tempdir().expect("root");
    let root = SecureDirectory::open_root(&absolute(root.path())).expect("open root");
    let metadata = root
        .child(".workflow-management")
        .expect("management directory");
    let receipts = metadata.child("receipts").expect("receipts directory");
    receipts
        .write_file("receipt.json", b"first", /*replace*/ false)
        .expect("create receipt");
    assert_eq!(
        receipts
            .read_file("receipt.json", /*maximum_bytes*/ 5)
            .expect("read receipt"),
        b"first"
    );
    assert!(
        receipts
            .write_file("receipt.json", b"collision", /*replace*/ false)
            .is_err()
    );
    receipts
        .write_file("receipt.json", b"next", /*replace*/ true)
        .expect("replace receipt");
    assert_eq!(
        receipts
            .read_file("receipt.json", /*maximum_bytes*/ 4)
            .expect("read replaced receipt"),
        b"next"
    );
    assert!(
        receipts
            .read_file("receipt.json", /*maximum_bytes*/ 3)
            .is_err()
    );
    assert!(metadata.child("receipts").is_ok());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(metadata.path().as_path())
                .expect("directory mode")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(receipts.path().join("receipt.json").as_path())
                .expect("file mode")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn no_replace_directory_rename_preserves_existing_target() {
    let root = tempfile::tempdir().expect("root");
    let root = SecureDirectory::open_root(&absolute(root.path())).expect("open root");
    let staging = root.child("staging").expect("staging");
    let active = root.child("active").expect("active");
    staging.child("candidate").expect("candidate");
    staging
        .rename_child_noreplace("candidate", &active, "workflow")
        .expect("publish candidate");
    assert!(active.path().join("workflow").is_dir());
    staging.child("another").expect("another candidate");
    assert!(
        staging
            .rename_child_noreplace("another", &active, "workflow")
            .is_err()
    );
    assert!(staging.path().join("another").is_dir());
}

#[cfg(unix)]
#[test]
fn rejects_aliased_metadata_and_world_accessible_directory() {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().expect("root");
    let root = SecureDirectory::open_root(&absolute(root.path())).expect("open root");
    let private = root.child("private").expect("private");
    private
        .write_file("real", b"contents", /*replace*/ false)
        .expect("real file");
    symlink("real", private.path().join("alias").as_path()).expect("alias");
    assert!(private.read_file("alias", /*maximum_bytes*/ 8).is_err());
    let status = std::process::Command::new("mkfifo")
        .arg(private.path().join("fifo").as_path())
        .status()
        .expect("create FIFO");
    assert!(status.success(), "mkfifo failed");
    assert!(private.read_file("fifo", /*maximum_bytes*/ 8).is_err());
    fs::set_permissions(private.path().as_path(), fs::Permissions::from_mode(0o755))
        .expect("relax directory mode");
    assert!(root.child("private").is_err());
}

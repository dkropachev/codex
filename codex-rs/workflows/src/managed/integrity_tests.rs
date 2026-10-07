use std::fs;
use std::time::Duration;

use pretty_assertions::assert_eq;

use super::*;

fn scan(
    root: &Path,
    limits: VerificationLimits,
    kind: PayloadKind,
) -> anyhow::Result<PayloadInventory> {
    scan_payload(
        root,
        kind,
        limits,
        crate::runner::CommandDeadline::after(Duration::from_secs(/*secs*/ 2)),
        /*cancelled*/ None,
    )
}

#[test]
fn payload_scan_is_sorted_and_limits_are_inclusive() {
    let root = tempfile::tempdir().expect("root");
    fs::create_dir(root.path().join("empty")).expect("empty directory");
    fs::create_dir(root.path().join(".git")).expect("checkout metadata");
    fs::write(root.path().join("z"), b"ab").expect("first file");
    fs::write(root.path().join("a"), b"c").expect("second file");
    let mut limits = super::super::fetch::VERIFICATION_LIMITS;
    limits.post_install_entries = 3;
    limits.post_install_bytes = 3;
    let inventory = scan(root.path(), limits, PayloadKind::Source).expect("exact limits");
    assert_eq!(
        inventory
            .entries
            .iter()
            .map(|entry| entry.path.as_str())
            .collect::<Vec<_>>(),
        ["a", "empty", "z"]
    );
    assert_eq!(inventory.logical_bytes, 3);
    assert!(scan(root.path(), limits, PayloadKind::Installed).is_err());
    limits.post_install_entries = 2;
    assert!(scan(root.path(), limits, PayloadKind::Source).is_err());
    limits.post_install_entries = 3;
    limits.post_install_bytes = 2;
    assert!(scan(root.path(), limits, PayloadKind::Source).is_err());
}

#[cfg(unix)]
#[test]
fn scan_rejects_special_files_and_dependency_root_alias() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().expect("root");
    let status = std::process::Command::new("mkfifo")
        .arg(root.path().join("fifo"))
        .status()
        .expect("create FIFO");
    assert!(status.success());
    assert!(
        scan(
            root.path(),
            super::super::fetch::VERIFICATION_LIMITS,
            PayloadKind::Source
        )
        .is_err()
    );
    fs::remove_file(root.path().join("fifo")).expect("remove FIFO");
    fs::create_dir(root.path().join("external")).expect("external directory");
    symlink("external", root.path().join("node_modules")).expect("alias dependency root");
    assert!(
        scan(
            root.path(),
            super::super::fetch::VERIFICATION_LIMITS,
            PayloadKind::Installed
        )
        .is_err()
    );
}

#[cfg(unix)]
#[test]
fn scan_rejects_case_colliding_directory_prefixes() {
    for (first, second) in [("Foo/a", "foo/b"), ("a", "A/b")] {
        let root = tempfile::tempdir().expect("root");
        if let Some(parent) = root.path().join(first).parent() {
            fs::create_dir_all(parent).expect("first parent");
        }
        fs::write(root.path().join(first), b"first").expect("first file");
        fs::create_dir_all(root.path().join(second).parent().expect("second parent"))
            .expect("second parent directory");
        fs::write(root.path().join(second), b"second").expect("second file");
        assert!(
            scan(
                root.path(),
                super::super::fetch::VERIFICATION_LIMITS,
                PayloadKind::Source
            )
            .is_err()
        );
    }
}

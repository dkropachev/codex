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

#[cfg(unix)]
#[test]
fn dependency_links_must_resolve_inside_node_modules() {
    use std::os::unix::fs::symlink;

    for (target, accepted) in [
        ("../pkg/bin.js", true),
        ("../../outside", false),
        ("../missing", false),
        ("/tmp", false),
        ("cycle", false),
    ] {
        let root = tempfile::tempdir().expect("root");
        let modules = root.path().join("node_modules");
        fs::create_dir_all(modules.join("pkg")).expect("package");
        fs::create_dir(modules.join(".bin")).expect("bin");
        fs::write(modules.join("pkg/bin.js"), b"bin").expect("target");
        fs::write(root.path().join("outside"), b"outside").expect("outside");
        symlink(target, modules.join(".bin/tool")).expect("dependency link");
        if target == "cycle" {
            symlink("tool", modules.join(".bin/cycle")).expect("cycle link");
        }
        assert_eq!(
            scan(
                root.path(),
                super::super::fetch::VERIFICATION_LIMITS,
                PayloadKind::Installed
            )
            .is_ok(),
            accepted,
            "{target}"
        );
    }
}

#[cfg(windows)]
#[test]
fn windows_dependency_links_accept_contained_targets_and_reject_junctions() {
    use std::os::windows::fs::symlink_file;

    let root = tempfile::tempdir().expect("root");
    let modules = root.path().join("node_modules");
    fs::create_dir_all(modules.join("pkg")).expect("package");
    fs::create_dir(modules.join(".bin")).expect("bin");
    fs::write(modules.join("pkg/bin.js"), b"bin").expect("target");
    let link = modules.join(".bin/tool");
    if symlink_file(r"..\pkg\bin.js", &link).is_ok() {
        assert!(
            scan(
                root.path(),
                super::super::fetch::VERIFICATION_LIMITS,
                PayloadKind::Installed
            )
            .is_ok()
        );
        fs::remove_file(&link).expect("remove contained link");
    }

    let outside = root.path().join("outside");
    fs::create_dir(&outside).expect("outside");
    let junction = modules.join("junction");
    let output = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&junction)
        .arg(&outside)
        .output()
        .expect("create junction");
    assert!(output.status.success(), "mklink /J failed");
    assert!(
        scan(
            root.path(),
            super::super::fetch::VERIFICATION_LIMITS,
            PayloadKind::Installed
        )
        .is_err()
    );
    fs::remove_dir(junction).expect("remove junction");
}

#[test]
fn index_parser_requires_stage_zero_and_valid_modes() {
    let oid = "a".repeat(40);
    assert_eq!(
        parse_index(
            format!("100644 {oid} 0\ta\0").as_bytes(),
            /*oid_length*/ 40
        )
        .expect("valid index"),
        [IndexEntry {
            path: "a".into(),
            mode: "100644".into(),
            oid
        }]
    );
    assert!(parse_index(b"100644 deadbeef 1\ta\0", /*oid_length*/ 8).is_err());
    assert!(parse_index(b"120000 deadbeef 0\ta\0", /*oid_length*/ 8).is_err());
}

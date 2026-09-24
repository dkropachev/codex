use std::fs;

use tempfile::TempDir;

use super::*;

fn request() -> ScaffoldRequest {
    ScaffoldRequest {
        id: "reports/daily".to_string(),
        title: "Daily report".to_string(),
        callable_name: "daily-report".to_string(),
        description: "Prepare the daily report.".to_string(),
    }
}

#[test]
fn scaffolds_complete_git_package_without_overwriting() {
    let temp = TempDir::new().expect("tempdir");
    let package = scaffold_workflow(temp.path(), &request()).expect("scaffold package");
    for relative in [
        "workflow.yaml",
        "package.json",
        ".gitignore",
        "README.md",
        "DESIGN.md",
        "src/workflow.ts",
        "src/tests/workflow.test.ts",
        "state/.gitkeep",
    ] {
        assert!(package.join(relative).is_file(), "missing {relative}");
    }
    assert!(package.join(".git").is_dir());

    let marker = package.join("marker");
    fs::write(&marker, "keep").expect("write marker");
    let error = scaffold_workflow(temp.path(), &request()).expect_err("reject collision");
    assert!(format!("{error:#}").contains("already exists"));
    assert_eq!(fs::read_to_string(marker).expect("read marker"), "keep");
}

#[cfg(unix)]
#[test]
fn refuses_to_traverse_symlinked_id_component() {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new().expect("tempdir");
    let outside = TempDir::new().expect("outside tempdir");
    fs::create_dir_all(temp.path()).expect("create root");
    symlink(outside.path(), temp.path().join("reports")).expect("create symlink");

    let error = scaffold_workflow(temp.path(), &request()).expect_err("reject symlink");
    assert!(format!("{error:#}").contains("symbolic link"));
    assert!(!outside.path().join("daily").exists());
}

#[cfg(unix)]
#[test]
fn refuses_a_symlink_in_the_workflow_root_path() {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new().expect("tempdir");
    let outside = TempDir::new().expect("outside tempdir");
    symlink(outside.path(), temp.path().join("linked-root")).expect("create root symlink");

    let error = scaffold_workflow(&temp.path().join("linked-root/workflows"), &request())
        .expect_err("reject root symlink");
    assert!(format!("{error:#}").contains("symbolic link"));
    assert!(!outside.path().join("workflows/reports/daily").exists());
}

#[cfg(all(unix, not(target_os = "redox")))]
#[test]
fn secure_parent_recheck_rejects_a_concurrent_symlink_swap() {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new().expect("tempdir");
    let root = temp.path().join("root");
    let outside = temp.path().join("outside");
    fs::create_dir_all(root.join("reports")).expect("create workflow parent");
    fs::create_dir(&outside).expect("create outside directory");
    let original =
        SecureWorkflowParent::open_or_create(&root, &["reports"]).expect("open original parent");
    let staging = SecureStagingDirectory::create(&original.directory)
        .expect("stage through directory descriptor");
    write_file_at(
        &staging.directory,
        std::path::Path::new("marker"),
        b"staged",
    )
    .expect("write staged marker");

    fs::rename(root.join("reports"), root.join("original-reports")).expect("move visible parent");
    symlink(&outside, root.join("reports")).expect("swap parent for symlink");

    let error = SecureWorkflowParent::open_or_create(&root, &["reports"])
        .expect_err("final parent recheck must reject the symlink");
    assert!(format!("{error:#}").contains("symbolic link"));
    assert!(!outside.join("daily").exists());
    assert!(
        rustix::fs::statat(
            &staging.directory,
            "marker",
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )
        .is_ok()
    );
}

#[test]
fn rejects_unsafe_ids() {
    for id in ["../escape", "/absolute", "UPPER", "a//b"] {
        assert!(normalize_workflow_id(id).is_err(), "accepted {id}");
    }
}

#[test]
#[cfg(not(all(unix, not(target_os = "redox"))))]
fn final_install_never_replaces_a_concurrently_created_empty_target() {
    let temp = TempDir::new().expect("tempdir");
    let staging = temp.path().join("staging");
    let target = temp.path().join("target");
    fs::create_dir(&staging).expect("create staging directory");
    fs::write(staging.join("marker"), "staged").expect("write staged marker");
    fs::create_dir(&target).expect("create concurrent target");

    install_staged_package(&staging, &target).expect_err("existing target must win");

    assert!(target.is_dir());
    assert_eq!(fs::read_dir(&target).expect("read target").count(), 0);
    assert_eq!(
        fs::read_to_string(staging.join("marker")).expect("staging remains recoverable"),
        "staged"
    );
}

#[cfg(all(unix, not(target_os = "redox")))]
#[test]
fn final_install_never_replaces_a_concurrently_created_empty_target() {
    let temp = TempDir::new().expect("tempdir");
    fs::create_dir(temp.path().join("target")).expect("create concurrent target");
    let parent =
        SecureWorkflowParent::open_or_create(temp.path(), &[]).expect("open secure parent");
    let mut staging =
        SecureStagingDirectory::create(&parent.directory).expect("create secure staging directory");
    write_file_at(
        &staging.directory,
        std::path::Path::new("marker"),
        b"staged",
    )
    .expect("write staged marker");

    staging
        .install_into(&parent.directory, "target")
        .expect_err("existing target must win");

    assert_eq!(
        fs::read_dir(temp.path().join("target"))
            .expect("read target")
            .count(),
        0
    );
    assert!(
        rustix::fs::statat(
            &staging.directory,
            "marker",
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )
        .is_ok()
    );
}

#[test]
fn rejects_generated_packages_that_exceed_loader_bounds_before_installing() {
    let temp = TempDir::new().expect("tempdir");
    let mut oversized = request();
    oversized.description = "x".repeat(crate::manifest::MAX_WORKFLOW_YAML_BYTES as usize);

    let error = scaffold_workflow(temp.path(), &oversized)
        .expect_err("oversized generated metadata must be rejected");

    assert!(format!("{error:#}").contains("65536-byte limit"));
    assert!(
        !workflow_path(temp.path(), &oversized.id)
            .expect("workflow path")
            .exists()
    );
}

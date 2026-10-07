use std::fs;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;

use super::*;

fn absolute(path: &Path) -> AbsolutePathBuf {
    AbsolutePathBuf::from_absolute_path_checked(path).expect("absolute fixture path")
}

fn deadline() -> crate::runner::CommandDeadline {
    crate::runner::CommandDeadline::after(Duration::from_secs(/*secs*/ 5))
}

#[test]
fn copies_regular_payload_and_contained_dependency_link_without_git() {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::fs::symlink;

    let source = tempfile::tempdir().expect("source");
    fs::create_dir_all(source.path().join("src/empty")).expect("empty directory");
    fs::create_dir_all(source.path().join("node_modules/.bin")).expect("dependency bins");
    fs::create_dir(source.path().join(".git")).expect("checkout metadata");
    fs::write(source.path().join(".git/config"), "ignored metadata").expect("git config");
    fs::write(source.path().join("src/run.sh"), "#!/bin/sh\nexit 0\n").expect("source file");
    fs::set_permissions(
        source.path().join("src/run.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .expect("executable mode");
    fs::write(source.path().join("node_modules/dep.js"), "dependency").expect("dependency");
    symlink("../dep.js", source.path().join("node_modules/.bin/dep"))
        .expect("contained dependency link");

    let target = tempfile::tempdir().expect("target");
    let target_dir = SecureDirectory::open_root(&absolute(target.path())).expect("target root");
    let mut limits = crate::managed::fetch::VERIFICATION_LIMITS;
    limits.post_install_entries = 7;
    copy_verified_payload(
        &absolute(source.path()),
        &target_dir,
        limits,
        deadline(),
        /*cancelled*/ None,
    )
    .expect("copy payload");
    let copied = target.path().join("payload");
    assert!(!copied.join(".git").exists());
    assert!(copied.join("src/empty").is_dir());
    assert_eq!(
        fs::read(copied.join("src/run.sh")).expect("copied file"),
        b"#!/bin/sh\nexit 0\n"
    );
    assert_eq!(
        fs::metadata(copied.join("src/run.sh"))
            .expect("copied mode")
            .permissions()
            .mode()
            & 0o111,
        0o100
    );
    assert_eq!(
        fs::read_link(copied.join("node_modules/.bin/dep")).expect("copied link"),
        Path::new("../dep.js")
    );
    limits.post_install_entries = 6;
    let too_small = tempfile::tempdir().expect("smaller target");
    let too_small_dir =
        SecureDirectory::open_root(&absolute(too_small.path())).expect("smaller target root");
    assert!(
        copy_verified_payload(
            &absolute(source.path()),
            &too_small_dir,
            limits,
            deadline(),
            /*cancelled*/ None,
        )
        .is_err()
    );
}

#[test]
fn rejects_aliased_source_directory_and_special_file_without_following() {
    use std::os::unix::fs::symlink;

    let source = tempfile::tempdir().expect("source");
    let external = tempfile::tempdir().expect("external");
    fs::write(external.path().join("secret"), "keep").expect("external file");
    symlink(external.path(), source.path().join("src")).expect("alias source directory");
    let target = tempfile::tempdir().expect("target");
    let target_dir = SecureDirectory::open_root(&absolute(target.path())).expect("target root");
    assert!(
        copy_verified_payload(
            &absolute(source.path()),
            &target_dir,
            crate::managed::fetch::VERIFICATION_LIMITS,
            deadline(),
            /*cancelled*/ None,
        )
        .is_err()
    );
    assert_eq!(
        fs::read(external.path().join("secret")).expect("external unchanged"),
        b"keep"
    );

    fs::remove_file(source.path().join("src")).expect("remove alias");
    let status = std::process::Command::new("mkfifo")
        .arg(source.path().join("fifo"))
        .status()
        .expect("create FIFO");
    assert!(status.success());
    let other = tempfile::tempdir().expect("other target");
    let other_dir = SecureDirectory::open_root(&absolute(other.path())).expect("other root");
    assert!(
        copy_verified_payload(
            &absolute(source.path()),
            &other_dir,
            crate::managed::fetch::VERIFICATION_LIMITS,
            deadline(),
            /*cancelled*/ None,
        )
        .is_err()
    );
}

#[test]
fn cancellation_and_byte_bound_stop_copy() {
    let source = tempfile::tempdir().expect("source");
    fs::write(source.path().join("file"), b"1234").expect("source file");
    let target = tempfile::tempdir().expect("target");
    let target_dir = SecureDirectory::open_root(&absolute(target.path())).expect("target root");
    let cancelled = AtomicBool::new(true);
    assert!(
        copy_verified_payload(
            &absolute(source.path()),
            &target_dir,
            crate::managed::fetch::VERIFICATION_LIMITS,
            deadline(),
            Some(&cancelled),
        )
        .is_err()
    );
    assert!(!target.path().join("payload").exists());

    let mut limits = crate::managed::fetch::VERIFICATION_LIMITS;
    limits.post_install_bytes = 3;
    assert!(
        copy_verified_payload(
            &absolute(source.path()),
            &target_dir,
            limits,
            deadline(),
            /*cancelled*/ None,
        )
        .is_err()
    );
}

#[test]
fn dependency_link_target_counts_toward_logical_byte_bound() {
    use std::os::unix::fs::symlink;

    let source = tempfile::tempdir().expect("source");
    fs::create_dir_all(source.path().join("node_modules/.bin")).expect("dependency bins");
    fs::write(source.path().join("node_modules/dep.js"), b"1234").expect("dependency file");
    symlink("../dep.js", source.path().join("node_modules/.bin/dep")).expect("dependency link");
    let mut limits = crate::managed::fetch::VERIFICATION_LIMITS;
    limits.post_install_bytes = 13;
    let exact = tempfile::tempdir().expect("exact target");
    let exact_dir = SecureDirectory::open_root(&absolute(exact.path())).expect("exact root");
    copy_verified_payload(
        &absolute(source.path()),
        &exact_dir,
        limits,
        deadline(),
        /*cancelled*/ None,
    )
    .expect("exact logical byte limit");

    limits.post_install_bytes = 12;
    let excess = tempfile::tempdir().expect("excess target");
    let excess_dir = SecureDirectory::open_root(&absolute(excess.path())).expect("excess root");
    assert!(
        copy_verified_payload(
            &absolute(source.path()),
            &excess_dir,
            limits,
            deadline(),
            /*cancelled*/ None,
        )
        .is_err()
    );
}

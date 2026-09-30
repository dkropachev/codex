use pretty_assertions::assert_eq;
use std::sync::atomic::AtomicBool;

use super::*;

const OID: &str = "1111111111111111111111111111111111111111";

#[test]
fn accepts_portable_regular_files_at_exact_limits() {
    let listing = listing(&[
        ("100644", "blob", "4", b"src/workflow.ts"),
        ("100755", "blob", "3", b"state/.gitkeep"),
    ]);
    assert_eq!(
        validate_tree_listing(
            &listing,
            /*object_id_bytes*/ 40,
            TreeLimits { files: 2, bytes: 7 },
            /*cancelled*/ None,
        )
        .map_err(|error| error.to_string()),
        Ok(())
    );

    let components = vec!["a".repeat(255); 16];
    let longest = components.join("/");
    assert_eq!(longest.len(), MAX_PATH_BYTES - 1);
    assert_eq!(portable_path(&longest).expect("portable path"), longest);
    let mut components = components;
    components[0].pop();
    let too_long = format!("{}/b", components.join("/"));
    assert_eq!(too_long.len(), MAX_PATH_BYTES);
    assert!(portable_path(&too_long).is_err());
}

#[test]
fn rejects_unsupported_entries_unsafe_paths_and_runtime_content() {
    for (mode, kind, size, path, expected) in [
        ("120000", "blob", "4", b"link".as_slice(), "symbolic link"),
        (
            "160000",
            "commit",
            "-",
            b"dependency".as_slice(),
            "submodule",
        ),
        (
            "100644",
            "blob",
            "1",
            b"../escape".as_slice(),
            "unsafe path",
        ),
        (
            "100644",
            "blob",
            "1",
            b".GIT/config".as_slice(),
            "unsafe path",
        ),
        (
            "100644",
            "blob",
            "1",
            b"git~1/config".as_slice(),
            "unsafe path",
        ),
        ("100644", "blob", "1", b"CON.txt".as_slice(), "unsafe path"),
        ("100644", "blob", "1", b"CONOUT$".as_slice(), "unsafe path"),
        ("100644", "blob", "1", b"COM0.log".as_slice(), "unsafe path"),
        ("100644", "blob", "1", b"LPT9".as_slice(), "unsafe path"),
        ("100644", "blob", "1", "COM¹.txt".as_bytes(), "unsafe path"),
        ("100644", "blob", "1", "café".as_bytes(), "unsafe path"),
        (
            "100644",
            "blob",
            "1",
            "ſtate/file".as_bytes(),
            "unsafe path",
        ),
        (
            "100644",
            "blob",
            "1",
            b"bad\\path".as_slice(),
            "unsafe path",
        ),
        (
            "100644",
            "blob",
            "1",
            b"/absolute".as_slice(),
            "unsafe path",
        ),
        (
            "100644",
            "blob",
            "1",
            b"trailing/".as_slice(),
            "unsafe path",
        ),
        (
            "100644",
            "blob",
            "1",
            b"empty//part".as_slice(),
            "unsafe path",
        ),
        (
            "100644",
            "blob",
            "1",
            b"dot/./part".as_slice(),
            "unsafe path",
        ),
        ("100644", "blob", "1", b"space ".as_slice(), "unsafe path"),
        ("100644", "blob", "1", b"dot.".as_slice(), "unsafe path"),
        ("100644", "blob", "1", b"bad:path".as_slice(), "unsafe path"),
        (
            "100644",
            "blob",
            "1",
            b"bad\npath".as_slice(),
            "unsafe path",
        ),
        (
            "100644",
            "blob",
            "1",
            b"state/session".as_slice(),
            "runtime path",
        ),
        (
            "100644",
            "blob",
            "1",
            b"artifacts/out".as_slice(),
            "runtime path",
        ),
        (
            "100644",
            "blob",
            "1",
            b"node_modules/pkg".as_slice(),
            "runtime path",
        ),
    ] {
        let error = validate_tree_listing(
            &listing(&[(mode, kind, size, path)]),
            /*object_id_bytes*/ 40,
            TREE_LIMITS,
            /*cancelled*/ None,
        )
        .expect_err("unsafe tree entry should fail");
        assert!(format!("{error:#}").contains(expected));
    }

    let non_utf8 = listing(&[("100644", "blob", "1", b"bad\xff")]);
    assert!(
        format!(
            "{:#}",
            validate_tree_listing(
                &non_utf8,
                /*object_id_bytes*/ 40,
                TREE_LIMITS,
                /*cancelled*/ None,
            )
            .expect_err("non-UTF-8 path should fail")
        )
        .contains("UTF-8")
    );
    assert!(portable_path("").is_err());
    assert!(portable_path(&"a".repeat(256)).is_err());
}

#[test]
fn rejects_malformed_tree_metadata() {
    let sha256 = "2".repeat(64);
    for (metadata, object_id_bytes) in [
        (format!("100644 blob {} 1", "0".repeat(40)), 40),
        (format!("100644 blob {} 1", "g".repeat(40)), 40),
        (format!("100644 blob {} 1", "1".repeat(39)), 40),
        (format!("100644 blob {sha256} nope"), 64),
        (format!("100644 blob {sha256}"), 64),
        (format!("100644 blob {sha256} 1 extra"), 64),
    ] {
        let error = validate_tree_listing(
            &raw_listing(&metadata, b"file"),
            object_id_bytes,
            TREE_LIMITS,
            /*cancelled*/ None,
        )
        .expect_err("malformed metadata should fail");
        assert!(
            format!("{error:#}").contains("invalid") || error.to_string().contains("malformed")
        );
    }

    let no_separator = format!("100644 blob {OID} 1\0");
    let error = validate_tree_listing(
        no_separator.as_bytes(),
        /*object_id_bytes*/ 40,
        TREE_LIMITS,
        /*cancelled*/ None,
    )
    .expect_err("missing separator should fail");
    assert!(error.to_string().contains("malformed"));

    let mut unterminated = raw_listing(&format!("100644 blob {OID} 1"), b"file");
    unterminated.pop();
    let error = validate_tree_listing(
        &unterminated,
        /*object_id_bytes*/ 40,
        TREE_LIMITS,
        /*cancelled*/ None,
    )
    .expect_err("unterminated record should fail");
    assert!(error.to_string().contains("unterminated"));
    let mut empty_record = raw_listing(&format!("100644 blob {OID} 1"), b"file");
    empty_record.push(0);
    let error = validate_tree_listing(
        &empty_record,
        /*object_id_bytes*/ 40,
        TREE_LIMITS,
        /*cancelled*/ None,
    )
    .expect_err("empty record should fail");
    assert!(error.to_string().contains("empty"));
}

#[test]
fn rejects_portable_collisions_and_over_limit_trees() {
    let collision = listing(&[
        ("100644", "blob", "1", b"Readme"),
        ("100644", "blob", "1", b"README"),
    ]);
    let error = validate_tree_listing(
        &collision,
        /*object_id_bytes*/ 40,
        TREE_LIMITS,
        /*cancelled*/ None,
    )
    .expect_err("portable collision should fail");
    assert!(error.to_string().contains("collide"));

    let two_files = listing(&[
        ("100644", "blob", "2", b"one"),
        ("100644", "blob", "2", b"two"),
    ]);
    for (limits, expected) in [
        (TreeLimits { files: 1, bytes: 4 }, "more than 1 files"),
        (TreeLimits { files: 2, bytes: 3 }, "more than 3 bytes"),
    ] {
        let error = validate_tree_listing(
            &two_files, /*object_id_bytes*/ 40, limits, /*cancelled*/ None,
        )
        .expect_err("tree limit should fail");
        assert!(format!("{error:#}").contains(expected));
    }

    let error = validate_tree_listing(
        &two_files,
        /*object_id_bytes*/ 40,
        TREE_LIMITS,
        Some(&AtomicBool::new(true)),
    )
    .expect_err("cancelled tree inspection should fail");
    assert!(format!("{error:#}").contains("cancelled"));
}

#[cfg(unix)]
#[test]
fn pre_cancelled_tree_verification_does_not_spawn_git() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().expect("temporary Git directory");
    let sentinel = root.path().join("spawned");
    let fake_git = root.path().join("git");
    std::fs::write(
        &fake_git,
        format!("#!/bin/sh\ntouch '{}'\n", sentinel.display()),
    )
    .expect("write fake Git");
    std::fs::set_permissions(&fake_git, std::fs::Permissions::from_mode(0o755))
        .expect("make fake Git executable");
    let error = verify_tracked_tree(
        fake_git.as_os_str(),
        root.path(),
        root.path(),
        OID,
        Some(&AtomicBool::new(true)),
    )
    .expect_err("pre-cancelled verification should fail");
    assert!(format!("{error:#}").contains("cancelled"));
    assert!(!sentinel.exists());
}

fn listing(entries: &[(&str, &str, &str, &[u8])]) -> Vec<u8> {
    let mut output = Vec::new();
    for (mode, kind, size, path) in entries {
        output.extend(raw_listing(&format!("{mode} {kind} {OID} {size}"), path));
    }
    output
}

fn raw_listing(metadata: &str, path: &[u8]) -> Vec<u8> {
    let mut output = format!("{metadata}\t").into_bytes();
    output.extend_from_slice(path);
    output.push(0);
    output
}

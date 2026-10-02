use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;

use super::*;

const PACKAGE_JSON: &[u8] = b"{ \"dependencies\": { \"dep\": \"1.0.0\" } }\n";
const BINARY_LOCK: &[u8] = b"\0binary\xfflock\n";

struct Fixture {
    _temporary: tempfile::TempDir,
    candidate: AbsolutePathBuf,
    environment: ManagedBunEnvironment,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().expect("temporary fixture");
        let candidate = temporary.path().join("candidate");
        fs::create_dir(&candidate).expect("create candidate");
        fs::write(candidate.join("package.json"), PACKAGE_JSON).expect("write package.json");
        fs::write(candidate.join("bun.lockb"), BINARY_LOCK).expect("write bun.lockb");
        let management = absolute(&temporary.path().join("management"));
        let environment = super::super::super::bun::materialize_bun_environment(&management)
            .expect("create Bun environment");
        Self {
            _temporary: temporary,
            candidate: absolute(&candidate),
            environment,
        }
    }

    fn stage(&self) -> anyhow::Result<()> {
        stage_binary_lock_inputs(&self.candidate, &self.environment)
    }

    fn input(&self, name: &str) -> AbsolutePathBuf {
        self.candidate.join(name)
    }

    fn scratch(&self, name: &str) -> AbsolutePathBuf {
        self.environment.scratch_dir.join(name)
    }

    fn scratch_contents(&self) -> BTreeMap<String, Vec<u8>> {
        fs::read_dir(self.environment.scratch_dir.as_path())
            .expect("read scratch")
            .map(|entry| {
                let entry = entry.expect("read scratch entry");
                (
                    entry.file_name().to_string_lossy().into_owned(),
                    fs::read(entry.path()).expect("read staged input"),
                )
            })
            .collect()
    }
}

fn absolute(path: &Path) -> AbsolutePathBuf {
    AbsolutePathBuf::from_absolute_path_checked(path).expect("absolute fixture path")
}

#[test]
fn stages_exact_bytes_and_only_the_two_inputs() {
    let fixture = Fixture::new();
    fixture.stage().expect("stage inputs");
    assert_eq!(
        fixture.scratch_contents(),
        BTreeMap::from([
            ("bun.lockb".to_string(), BINARY_LOCK.to_vec()),
            ("package.json".to_string(), PACKAGE_JSON.to_vec()),
        ])
    );
}

#[test]
fn nonempty_scratch_fails_without_partial_writes() {
    let fixture = Fixture::new();
    fs::write(fixture.scratch("sentinel").as_path(), b"unchanged").expect("write sentinel");
    let error = fixture.stage().expect_err("reject nonempty scratch");
    assert!(error.to_string().contains("scratch directory was not empty"));
    assert_eq!(
        fixture.scratch_contents(),
        BTreeMap::from([("sentinel".to_string(), b"unchanged".to_vec())])
    );
}

#[test]
fn rejects_directory_inputs() {
    for name in ["package.json", "bun.lockb"] {
        let fixture = Fixture::new();
        let input = fixture.input(name);
        fs::remove_file(input.as_path()).expect("remove input");
        fs::create_dir(input.as_path()).expect("create directory input");
        let error = fixture.stage().expect_err("reject directory input");
        assert!(
            error
                .to_string()
                .contains("must be a regular file without aliases"),
            "{name}: {error:#}"
        );
        assert_eq!(fixture.scratch_contents(), BTreeMap::new());
    }
}

#[cfg(unix)]
#[test]
fn rejects_special_input_files() {
    let fixture = Fixture::new();
    let input = fixture.input("bun.lockb");
    fs::remove_file(input.as_path()).expect("remove input");
    let _socket = std::os::unix::net::UnixListener::bind(input.as_path()).expect("bind socket");
    let error = fixture.stage().expect_err("reject special input");
    assert!(
        error
            .to_string()
            .contains("must be a regular file without aliases")
    );
    assert_eq!(fixture.scratch_contents(), BTreeMap::new());
}

#[cfg(any(unix, windows))]
#[test]
fn rejects_symlink_or_reparse_point_inputs() {
    for name in ["package.json", "bun.lockb"] {
        let fixture = Fixture::new();
        let input = fixture.input(name);
        let target = fixture.input(&format!("{name}.target"));
        fs::rename(input.as_path(), target.as_path()).expect("move input");
        if !create_file_alias(target.as_path(), input.as_path()) {
            return;
        }
        let error = fixture.stage().expect_err("reject aliased input");
        assert!(
            error
                .to_string()
                .contains("must be a regular file without aliases"),
            "{name}: {error:#}"
        );
        assert_eq!(fixture.scratch_contents(), BTreeMap::new());
    }
}

#[test]
fn input_size_limits_are_inclusive() {
    for (name, limit) in [
        ("package.json", crate::manifest::MAX_PACKAGE_JSON_BYTES),
        ("bun.lockb", MAX_BUN_LOCK_BYTES),
    ] {
        let oversized = Fixture::new();
        fs::File::create(oversized.input(name).as_path())
            .expect("replace input")
            .set_len(limit + 1)
            .expect("extend input");
        let error = oversized.stage().expect_err("reject oversized input");
        assert!(
            error
                .to_string()
                .contains(&format!("exceeds the {limit}-byte limit")),
            "{name}: {error:#}"
        );
        assert_eq!(oversized.scratch_contents(), BTreeMap::new());

        let exact = Fixture::new();
        fs::File::create(exact.input(name).as_path())
            .expect("replace input")
            .set_len(limit)
            .expect("extend input");
        exact.stage().expect("accept exact-size input");
        assert_eq!(
            fs::metadata(exact.scratch(name).as_path())
                .expect("staged input metadata")
                .len(),
            limit
        );
    }
}

#[test]
fn rejects_non_directory_scratch() {
    let fixture = Fixture::new();
    fs::remove_dir(fixture.environment.scratch_dir.as_path()).expect("remove scratch");
    fs::write(fixture.environment.scratch_dir.as_path(), "not a directory")
        .expect("replace scratch");
    let error = fixture.stage().expect_err("reject non-directory scratch");
    assert!(
        error
            .to_string()
            .contains("must be a regular directory without aliases")
    );
}

#[cfg(any(unix, windows))]
#[test]
fn rejects_aliased_scratch_directory() {
    let fixture = Fixture::new();
    let target = fixture.candidate.join("scratch-target");
    fs::rename(
        fixture.environment.scratch_dir.as_path(),
        target.as_path(),
    )
    .expect("move scratch directory");
    if !create_directory_alias(target.as_path(), fixture.environment.scratch_dir.as_path()) {
        return;
    }
    let error = fixture.stage().expect_err("reject aliased scratch");
    assert!(
        error
            .to_string()
            .contains("must be a regular directory without aliases")
    );
}

#[cfg(unix)]
fn create_file_alias(target: &Path, link: &Path) -> bool {
    std::os::unix::fs::symlink(target, link).expect("create file alias");
    true
}

#[cfg(unix)]
fn create_directory_alias(target: &Path, link: &Path) -> bool {
    std::os::unix::fs::symlink(target, link).expect("create directory alias");
    true
}

#[cfg(windows)]
fn create_file_alias(target: &Path, link: &Path) -> bool {
    create_windows_alias(std::os::windows::fs::symlink_file(target, link))
}

#[cfg(windows)]
fn create_directory_alias(target: &Path, link: &Path) -> bool {
    create_windows_alias(std::os::windows::fs::symlink_dir(target, link))
}

#[cfg(windows)]
fn create_windows_alias(result: std::io::Result<()>) -> bool {
    match result {
        Ok(()) => true,
        Err(error) if error.raw_os_error() == Some(1314) => false,
        Err(error) => panic!("failed to create alias: {error}"),
    }
}

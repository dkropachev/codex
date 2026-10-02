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
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let modes = [
            fixture
                .environment
                .scratch_dir
                .parent()
                .expect("operation root"),
            fixture.scratch("package.json"),
            fixture.scratch("bun.lockb"),
        ]
        .map(|path| fs::metadata(path).expect("metadata").permissions().mode() & 0o077);
        assert_eq!(modes, [0; 3]);
    }
}

#[test]
fn write_failure_removes_inputs_created_by_the_attempt() {
    let fixture = Fixture::new();
    let path = fixture.scratch("package.json");
    let error = write_staged_inputs(&[(path.clone(), PACKAGE_JSON), (path, BINARY_LOCK)])
        .expect_err("duplicate target should fail");
    assert!(error.to_string().contains("failed to stage"), "{error:#}");
    assert_eq!(fixture.scratch_contents(), BTreeMap::new());
}

#[test]
fn nonempty_scratch_fails_without_partial_writes() {
    let fixture = Fixture::new();
    fs::write(fixture.scratch("sentinel").as_path(), b"unchanged").expect("write sentinel");
    let error = fixture.stage().expect_err("reject nonempty scratch");
    assert!(
        error
            .to_string()
            .contains("scratch directory was not empty")
    );
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
fn rejects_fifo_inputs_without_blocking() {
    use rustix::fs::CWD;
    use rustix::fs::Mode;
    use rustix::fs::mkfifoat;

    let fixture = Fixture::new();
    let input = fixture.input("bun.lockb");
    fs::remove_file(input.as_path()).expect("remove input");
    mkfifoat(CWD, input.as_path(), Mode::RUSR | Mode::WUSR).expect("create FIFO");
    let error = fixture.stage().expect_err("reject special input");
    assert!(
        error
            .to_string()
            .contains("must be a regular file without aliases")
    );
    assert_eq!(fixture.scratch_contents(), BTreeMap::new());
}

#[cfg(unix)]
#[test]
fn rejects_symlink_inputs() {
    for name in ["package.json", "bun.lockb"] {
        let fixture = Fixture::new();
        let input = fixture.input(name);
        let target = fixture.input(&format!("{name}.target"));
        fs::rename(input.as_path(), target.as_path()).expect("move input");
        std::os::unix::fs::symlink(target.as_path(), input.as_path()).expect("create file alias");
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

#[cfg(windows)]
#[test]
fn rejects_reparse_point_inputs() {
    let fixture = Fixture::new();
    let input = fixture.input("bun.lockb");
    let target = fixture.input("bun.lockb.target");
    fs::remove_file(input.as_path()).expect("remove input");
    fs::create_dir(target.as_path()).expect("create junction target");
    create_directory_alias(target.as_path(), input.as_path());
    let metadata = super::super::file::open_no_follow(input.as_path())
        .expect("open junction itself")
        .metadata()
        .expect("junction metadata");
    assert!(super::super::file::is_windows_reparse_point(&metadata));
    let error = fixture.stage().expect_err("reject reparse-point input");
    assert!(
        error
            .to_string()
            .contains("must be a regular file without aliases")
    );
    remove_directory_alias(input.as_path());
    assert_eq!(fixture.scratch_contents(), BTreeMap::new());
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
    fs::rename(fixture.environment.scratch_dir.as_path(), target.as_path())
        .expect("move scratch directory");
    create_directory_alias(target.as_path(), fixture.environment.scratch_dir.as_path());
    let error = fixture.stage().expect_err("reject aliased scratch");
    assert!(
        error
            .to_string()
            .contains("must be a regular directory without aliases")
    );
    remove_directory_alias(fixture.environment.scratch_dir.as_path());
}

#[cfg(unix)]
fn create_directory_alias(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).expect("create directory alias");
}

#[cfg(unix)]
fn remove_directory_alias(link: &Path) {
    fs::remove_file(link).expect("remove directory alias");
}

#[cfg(windows)]
fn create_directory_alias(target: &Path, link: &Path) {
    let output = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .expect("create directory junction");
    assert!(output.status.success(), "mklink /J failed");
}

#[cfg(windows)]
fn remove_directory_alias(link: &Path) {
    fs::remove_dir(link).expect("remove directory junction");
}

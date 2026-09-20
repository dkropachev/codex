use std::collections::HashMap;
use std::collections::VecDeque;
use std::io;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::Mutex;

use anyhow::Context;
use anyhow::Result;
use codex_file_system::CopyOptions;
use codex_file_system::CreateDirectoryOptions;
use codex_file_system::ExecutorFileSystem;
use codex_file_system::ExecutorFileSystemFuture;
use codex_file_system::FileMetadata;
use codex_file_system::FileSystemReadStream;
use codex_file_system::FileSystemSandboxContext;
use codex_file_system::ReadDirectoryEntry;
use codex_file_system::RemoveOptions;
use codex_file_system::WalkOptions;
use codex_file_system::WalkOutcome;
use codex_utils_path_uri::PathUri;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::*;
use crate::ReviewCommand;
use crate::ReviewCommandOutput;

#[tokio::test]
async fn captures_exact_head_and_index_without_mutation() {
    let repository = TestRepository::new();
    repository.write("tracked.txt", "before\n");
    repository.git(&["add", "."]);
    repository.git(&["commit", "-m", "initial"]);
    repository.write("tracked.txt", "staged\n");
    repository.git(&["add", "tracked.txt"]);
    let root = PathUri::from_host_native_path(repository.path()).expect("repository URI");
    let raw_index = std::fs::read(repository.path().join(".git/index")).expect("read index");

    let snapshot = capture_review_fix_commit_snapshot(
        Arc::new(NativeRunner),
        Arc::new(TestFileSystem::native()),
        &root,
    )
    .await
    .expect("capture snapshot");

    assert_eq!(snapshot.head_sha, repository.git(&["rev-parse", "HEAD"]));
    assert_eq!(snapshot.index_contents, raw_index);
    assert_eq!(
        std::fs::read(repository.path().join(".git/index")).expect("read index"),
        raw_index
    );
    assert_eq!(temporary_files(repository.path()), Vec::<String>::new());
}

#[tokio::test]
async fn windows_snapshot_uses_remote_paths() {
    const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const TREE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const INDEX_TREE: &str = "cccccccccccccccccccccccccccccccccccccccc";
    const EMPTY_TREE: &str = "dddddddddddddddddddddddddddddddddddddddd";
    let root = PathUri::parse("file:///C:/workspace").expect("root URI");
    let index = root
        .join(".git\\work\ntrees\\review\\index")
        .expect("index URI");
    let fs = Arc::new(TestFileSystem::memory(index, b"raw-index".to_vec()));
    let runner = Arc::new(ScriptedRunner::new(
        root.clone(),
        vec![
            success(&["rev-parse", "--verify", "HEAD^{commit}"], HEAD),
            success(
                &["rev-parse", "--verify", &format!("{HEAD}^{{tree}}")],
                TREE,
            ),
            success(&["symbolic-ref", "-q", "HEAD"], "refs/heads/main"),
            success(
                &["rev-parse", "--git-path", "index"],
                ".git\\work\ntrees\\review\\index",
            ),
            success(&["mktree"], EMPTY_TREE),
            success(&["write-tree"], INDEX_TREE),
            success(&["rev-parse", "--verify", "HEAD^{commit}"], HEAD),
            success(&["symbolic-ref", "-q", "HEAD"], "refs/heads/main"),
        ],
    ));

    let snapshot = capture_review_fix_commit_snapshot(Arc::clone(&runner), fs.clone(), &root)
        .await
        .expect("capture Windows snapshot");

    assert_eq!(snapshot.index_tree, INDEX_TREE);
    assert!(
        fs.files()
            .keys()
            .all(|path| !path.to_string().contains(TEMP_FILE_PREFIX))
    );
}

struct NativeRunner;

impl ReviewSnapshotCommandRunner for NativeRunner {
    async fn run(&self, command: ReviewCommand) -> Result<ReviewCommandOutput> {
        let cwd = command.cwd().to_abs_path()?;
        let (program, args) = command.argv().split_first().context("empty command")?;
        let output = Command::new(program)
            .args(args)
            .current_dir(cwd.as_path())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .envs(command.env_vars())
            .output()?;
        Ok(ReviewCommandOutput {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8(output.stdout).context("test stdout was not UTF-8")?,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

struct TestRepository(TempDir);

impl TestRepository {
    fn new() -> Self {
        let repository = Self(tempfile::tempdir().expect("tempdir"));
        repository.git(&["init", "--quiet", "--initial-branch=main"]);
        repository.git(&["config", "user.name", "Review Test"]);
        repository.git(&["config", "user.email", "review@example.com"]);
        repository
    }

    fn path(&self) -> &Path {
        self.0.path()
    }

    fn write(&self, path: &str, contents: &str) {
        std::fs::write(self.path().join(path), contents).expect("write file");
    }

    fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(self.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }
}

fn temporary_files(repository: &Path) -> Vec<String> {
    std::fs::read_dir(repository.join(".git"))
        .expect("read Git directory")
        .filter_map(|entry| {
            let name = entry.ok()?.file_name().to_string_lossy().into_owned();
            name.starts_with(TEMP_FILE_PREFIX).then_some(name)
        })
        .collect()
}

fn command_args(command: &ReviewCommand) -> Vec<String> {
    command.argv()[5..].to_vec()
}

struct ExpectedCommand {
    args: Vec<String>,
    output: ReviewCommandOutput,
}

fn success(args: &[&str], stdout: &str) -> ExpectedCommand {
    ExpectedCommand {
        args: args.iter().map(|arg| (*arg).to_string()).collect(),
        output: ReviewCommandOutput {
            exit_code: 0,
            stdout: format!("{stdout}\n"),
            stderr: String::new(),
        },
    }
}

struct ScriptedRunner {
    root: PathUri,
    expected: Mutex<VecDeque<ExpectedCommand>>,
}

impl ScriptedRunner {
    fn new(root: PathUri, expected: Vec<ExpectedCommand>) -> Self {
        Self {
            root,
            expected: Mutex::new(expected.into()),
        }
    }
}

impl ReviewSnapshotCommandRunner for ScriptedRunner {
    async fn run(&self, command: ReviewCommand) -> Result<ReviewCommandOutput> {
        assert_eq!(command.cwd(), &self.root);
        assert_eq!(command.argv()[2], "core.hooksPath=NUL");
        let expected = self
            .expected
            .lock()
            .expect("expected lock")
            .pop_front()
            .expect("unexpected command");
        assert_eq!(command_args(&command), expected.args);
        Ok(expected.output)
    }
}

struct TestFileSystem {
    memory: Option<Mutex<HashMap<PathUri, Vec<u8>>>>,
}

impl TestFileSystem {
    fn native() -> Self {
        Self { memory: None }
    }

    fn memory(path: PathUri, contents: Vec<u8>) -> Self {
        Self {
            memory: Some(Mutex::new(HashMap::from([(path, contents)]))),
        }
    }

    fn files(&self) -> HashMap<PathUri, Vec<u8>> {
        self.memory
            .as_ref()
            .expect("memory filesystem")
            .lock()
            .expect("files lock")
            .clone()
    }
}

impl ExecutorFileSystem for TestFileSystem {
    fn canonicalize<'a>(
        &'a self,
        _: &'a PathUri,
        _: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, PathUri> {
        unimplemented!()
    }

    fn read_file<'a>(
        &'a self,
        path: &'a PathUri,
        _: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, Vec<u8>> {
        Box::pin(async move {
            if let Some(files) = &self.memory {
                files
                    .lock()
                    .expect("files lock")
                    .get(path)
                    .cloned()
                    .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, path.to_string()))
            } else {
                std::fs::read(path.to_abs_path()?.as_path())
            }
        })
    }

    fn read_file_stream<'a>(
        &'a self,
        _: &'a PathUri,
        _: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, FileSystemReadStream> {
        unimplemented!()
    }

    fn write_file<'a>(
        &'a self,
        path: &'a PathUri,
        contents: Vec<u8>,
        _: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        Box::pin(async move {
            if let Some(files) = &self.memory {
                files
                    .lock()
                    .expect("files lock")
                    .insert(path.clone(), contents);
                Ok(())
            } else {
                std::fs::write(path.to_abs_path()?.as_path(), contents)
            }
        })
    }

    fn create_directory<'a>(
        &'a self,
        _: &'a PathUri,
        _: CreateDirectoryOptions,
        _: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        unimplemented!()
    }

    fn get_metadata<'a>(
        &'a self,
        _: &'a PathUri,
        _: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, FileMetadata> {
        unimplemented!()
    }

    fn read_directory<'a>(
        &'a self,
        _: &'a PathUri,
        _: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, Vec<ReadDirectoryEntry>> {
        unimplemented!()
    }

    fn walk<'a>(
        &'a self,
        _: &'a PathUri,
        _: WalkOptions,
        _: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, WalkOutcome> {
        unimplemented!()
    }

    fn remove<'a>(
        &'a self,
        path: &'a PathUri,
        options: RemoveOptions,
        _: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        Box::pin(async move {
            if let Some(files) = &self.memory {
                files.lock().expect("files lock").remove(path);
                Ok(())
            } else {
                match std::fs::remove_file(path.to_abs_path()?.as_path()) {
                    Ok(()) => Ok(()),
                    Err(error) if options.force && error.kind() == io::ErrorKind::NotFound => {
                        Ok(())
                    }
                    Err(error) => Err(error),
                }
            }
        })
    }

    fn copy<'a>(
        &'a self,
        _: &'a PathUri,
        _: &'a PathUri,
        _: CopyOptions,
        _: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        unimplemented!()
    }
}

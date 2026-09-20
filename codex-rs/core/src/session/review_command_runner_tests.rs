use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use codex_exec_server::ByteChunk;
use codex_exec_server::ExecBackendFuture;
use codex_exec_server::ExecProcess;
use codex_exec_server::ExecProcessEventReceiver;
use codex_exec_server::ExecProcessFuture;
use codex_exec_server::ProcessOutputChunk;
use codex_exec_server::ReadResponse;
use codex_exec_server::StartedExecProcess;
use codex_git_utils::resolve_pull_request_for_review_with_runner;
use codex_utils_path_uri::PathUri;
use pretty_assertions::assert_eq;
use tokio::sync::watch;

use super::*;

#[test]
fn caps_each_output_stream_independently() {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    append_capped(&mut stdout, b"abcdef", /*output_bytes_cap*/ 4);
    append_capped(&mut stderr, b"uvwxyz", /*output_bytes_cap*/ 4);
    append_capped(&mut stdout, b"ignored", /*output_bytes_cap*/ 4);

    assert_eq!(stdout, b"abcd");
    assert_eq!(stderr, b"uvwx");
}

#[tokio::test]
async fn collector_caps_each_output_stream() {
    let process = TestProcess::new(ReadBehavior::Response(ReadResponse {
        chunks: vec![
            ProcessOutputChunk {
                seq: 1,
                stream: ExecOutputStream::Stdout,
                chunk: ByteChunk::from(b"abcdef".to_vec()),
            },
            ProcessOutputChunk {
                seq: 2,
                stream: ExecOutputStream::Stderr,
                chunk: ByteChunk::from(b"uvwxyz".to_vec()),
            },
        ],
        next_seq: 3,
        exited: true,
        exit_code: Some(0),
        closed: true,
        failure: None,
        sandbox_denied: false,
    }));

    assert_eq!(
        collect_output(&process, /*output_bytes_cap*/ 4)
            .await
            .expect("bounded output"),
        ReviewCommandOutput {
            exit_code: 0,
            stdout: "abcd".to_string(),
            stderr: "uvwx".to_string(),
        }
    );
}

#[tokio::test]
async fn runner_terminates_process_after_read_error_or_missing_exit_code() {
    for behavior in [
        ReadBehavior::Error,
        ReadBehavior::Response(ReadResponse {
            chunks: Vec::new(),
            next_seq: 0,
            exited: true,
            exit_code: None,
            closed: true,
            failure: None,
            sandbox_denied: false,
        }),
    ] {
        let process = Arc::new(TestProcess::new(behavior));
        let runner = ExecutorReviewCommandRunner::new(
            Arc::new(SingleProcessBackend::new(process.clone())),
            &ShellEnvironmentPolicy::default(),
        );

        resolve_pull_request_for_review_with_runner(
            &runner,
            &PathUri::parse("file:///workspace").expect("cwd"),
            "https://github.com/openai/codex/pull/42",
        )
        .await
        .expect_err("process collection failure");

        assert!(process.terminated.load(Ordering::SeqCst));
    }
}

#[tokio::test(start_paused = true)]
async fn runner_terminates_process_after_command_timeout() {
    let process = Arc::new(TestProcess::new(ReadBehavior::Hang));
    let runner = ExecutorReviewCommandRunner::new(
        Arc::new(SingleProcessBackend::new(process.clone())),
        &ShellEnvironmentPolicy::default(),
    );

    let error = resolve_pull_request_for_review_with_runner(
        &runner,
        &PathUri::parse("file:///workspace").expect("cwd"),
        "https://github.com/openai/codex/pull/42",
    )
    .await
    .expect_err("command timeout");

    assert!(format!("{error:#}").contains("timed out"));
    assert!(process.terminated.load(Ordering::SeqCst));
}

#[tokio::test]
async fn dropping_a_running_command_terminates_its_process() {
    let process = Arc::new(TestProcess::new(ReadBehavior::Hang));
    let runner = ExecutorReviewCommandRunner::new(
        Arc::new(SingleProcessBackend::new(process.clone())),
        &ShellEnvironmentPolicy::default(),
    );
    let cwd = PathUri::parse("file:///workspace").expect("cwd");
    let task = tokio::spawn(async move {
        resolve_pull_request_for_review_with_runner(
            &runner,
            &cwd,
            "https://github.com/openai/codex/pull/42",
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        while !process.read_started.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("command should start reading");

    task.abort();
    let _ = task.await;

    tokio::time::timeout(Duration::from_secs(1), async {
        while !process.terminated.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("dropped command should terminate");
}

#[test]
fn preserves_shell_environment_policy_for_executor() {
    let policy = ShellEnvironmentPolicy {
        inherit: codex_protocol::config_types::ShellEnvironmentPolicyInherit::Core,
        ignore_default_excludes: false,
        exclude: vec![
            codex_protocol::config_types::EnvironmentVariablePattern::new_case_insensitive(
                "PRIVATE_*",
            ),
        ],
        r#set: HashMap::from([
            ("PATH".to_string(), "/tools".to_string()),
            ("GH_REPO".to_string(), "unrelated/repo".to_string()),
            ("GIT_DIR".to_string(), "/other/.git".to_string()),
        ]),
        include_only: vec![
            codex_protocol::config_types::EnvironmentVariablePattern::new_case_insensitive("PATH"),
        ],
        use_profile: true,
    };

    assert_eq!(
        exec_env_policy(&policy),
        ExecEnvPolicy {
            inherit: codex_protocol::config_types::ShellEnvironmentPolicyInherit::Core,
            ignore_default_excludes: false,
            exclude: vec![
                "PRIVATE_*".to_string(),
                "GIT_*".to_string(),
                "GH_REPO".to_string(),
            ],
            r#set: HashMap::from([("PATH".to_string(), "/tools".to_string())]),
            include_only: vec!["PATH".to_string()],
        }
    );
    let sanitized = sanitized_review_shell_environment_policy(&policy);
    assert!(
        sanitized
            .exclude
            .iter()
            .any(|pattern| pattern.matches("GIT_DIR"))
    );
    assert!(
        sanitized
            .exclude
            .iter()
            .any(|pattern| pattern.matches("GH_REPO"))
    );
}

#[tokio::test]
async fn resolves_pull_request_at_foreign_executor_cwd() {
    let cwd = PathUri::parse("file:///C:/workspace").expect("foreign cwd URI");
    let backend = Arc::new(FakeBackend::new(
        cwd.clone(),
        vec![
            expected(
                &[
                    "gh",
                    "pr",
                    "view",
                    "https://github.com/openai/codex/pull/42",
                    "--json",
                    "number,title,body,url,state,baseRefName,baseRefOid,headRefOid",
                ],
                &serde_json::json!({
                    "number": 42,
                    "title": "Remote review",
                    "body": "Intent",
                    "url": "https://github.com/openai/codex/pull/42",
                    "state": "OPEN",
                    "baseRefName": "main",
                    "baseRefOid": "base-oid",
                    "headRefOid": "head-oid",
                })
                .to_string(),
            ),
            expected(
                &[
                    "git",
                    "rev-parse",
                    "--verify",
                    "--end-of-options",
                    "base-oid^{commit}",
                ],
                "resolved-base\n",
            ),
            expected(
                &["git", "merge-base", "HEAD", "resolved-base"],
                "merge-base\n",
            ),
        ],
    ));
    let runner =
        ExecutorReviewCommandRunner::new(backend.clone(), &ShellEnvironmentPolicy::default());

    let resolved = resolve_pull_request_for_review_with_runner(
        &runner,
        &cwd,
        "https://github.com/openai/codex/pull/42",
    )
    .await
    .expect("remote pull request scope");

    assert_eq!(resolved.merge_base, "merge-base");
    backend.assert_finished();
}

fn expected(argv: &[&str], stdout: &str) -> ExpectedCommand {
    ExpectedCommand {
        argv: argv.iter().map(|arg| (*arg).to_string()).collect(),
        stdout: stdout.to_string(),
    }
}

struct ExpectedCommand {
    argv: Vec<String>,
    stdout: String,
}

struct FakeBackend {
    cwd: PathUri,
    expected: StdMutex<VecDeque<ExpectedCommand>>,
}

impl FakeBackend {
    fn new(cwd: PathUri, expected: Vec<ExpectedCommand>) -> Self {
        Self {
            cwd,
            expected: StdMutex::new(expected.into()),
        }
    }

    fn assert_finished(&self) {
        assert!(self.expected.lock().expect("expected lock").is_empty());
    }
}

impl ExecBackend for FakeBackend {
    fn start(&self, params: ExecParams) -> ExecBackendFuture<'_> {
        Box::pin(async move {
            assert_eq!(params.cwd, self.cwd);
            assert_eq!(
                params.env_policy.as_ref().map(|policy| &policy.inherit),
                Some(&codex_protocol::config_types::ShellEnvironmentPolicyInherit::All)
            );
            assert_eq!(
                params.env.get("GIT_TERMINAL_PROMPT"),
                Some(&"0".to_string())
            );
            let expected = self
                .expected
                .lock()
                .expect("expected lock")
                .pop_front()
                .expect("unexpected command");
            assert_eq!(params.argv, expected.argv);
            let process_id = params.process_id;
            Ok(StartedExecProcess {
                process: Arc::new(FakeProcess {
                    process_id,
                    response: ReadResponse {
                        chunks: vec![ProcessOutputChunk {
                            seq: 1,
                            stream: ExecOutputStream::Stdout,
                            chunk: ByteChunk::from(expected.stdout.into_bytes()),
                        }],
                        next_seq: 2,
                        exited: true,
                        exit_code: Some(0),
                        closed: true,
                        failure: None,
                        sandbox_denied: false,
                    },
                }),
            })
        })
    }
}

struct FakeProcess {
    process_id: ProcessId,
    response: ReadResponse,
}

impl ExecProcess for FakeProcess {
    fn process_id(&self) -> &ProcessId {
        &self.process_id
    }

    fn subscribe_wake(&self) -> watch::Receiver<u64> {
        watch::channel(0).1
    }

    fn subscribe_events(&self) -> ExecProcessEventReceiver {
        ExecProcessEventReceiver::empty()
    }

    fn read(
        &self,
        _after_seq: Option<u64>,
        _max_bytes: Option<usize>,
        _wait_ms: Option<u64>,
    ) -> ExecProcessFuture<'_, ReadResponse> {
        Box::pin(async { Ok(self.response.clone()) })
    }

    fn write(&self, _chunk: Vec<u8>) -> ExecProcessFuture<'_, codex_exec_server::WriteResponse> {
        Box::pin(async { unreachable!("review commands do not write stdin") })
    }

    fn signal(&self, _signal: codex_exec_server::ProcessSignal) -> ExecProcessFuture<'_, ()> {
        Box::pin(async { unreachable!("review commands are not signalled in this test") })
    }

    fn terminate(&self) -> ExecProcessFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

enum ReadBehavior {
    Response(ReadResponse),
    Error,
    Hang,
}

struct TestProcess {
    process_id: ProcessId,
    behavior: ReadBehavior,
    terminated: AtomicBool,
    read_started: AtomicBool,
}

impl TestProcess {
    fn new(behavior: ReadBehavior) -> Self {
        Self {
            process_id: ProcessId::from("review-command-test".to_string()),
            behavior,
            terminated: AtomicBool::new(false),
            read_started: AtomicBool::new(false),
        }
    }
}

impl ExecProcess for TestProcess {
    fn process_id(&self) -> &ProcessId {
        &self.process_id
    }

    fn subscribe_wake(&self) -> watch::Receiver<u64> {
        watch::channel(0).1
    }

    fn subscribe_events(&self) -> ExecProcessEventReceiver {
        ExecProcessEventReceiver::empty()
    }

    fn read(
        &self,
        _after_seq: Option<u64>,
        _max_bytes: Option<usize>,
        _wait_ms: Option<u64>,
    ) -> ExecProcessFuture<'_, ReadResponse> {
        Box::pin(async move {
            self.read_started.store(true, Ordering::SeqCst);
            match &self.behavior {
                ReadBehavior::Response(response) => Ok(response.clone()),
                ReadBehavior::Error => Err(codex_exec_server::ExecServerError::Protocol(
                    "read failed".to_string(),
                )),
                ReadBehavior::Hang => std::future::pending().await,
            }
        })
    }

    fn write(&self, _chunk: Vec<u8>) -> ExecProcessFuture<'_, codex_exec_server::WriteResponse> {
        Box::pin(async { unreachable!("review commands do not write stdin") })
    }

    fn signal(&self, _signal: codex_exec_server::ProcessSignal) -> ExecProcessFuture<'_, ()> {
        Box::pin(async { unreachable!("review commands are not signalled in this test") })
    }

    fn terminate(&self) -> ExecProcessFuture<'_, ()> {
        Box::pin(async move {
            self.terminated.store(true, Ordering::SeqCst);
            Ok(())
        })
    }
}

struct SingleProcessBackend {
    process: Arc<TestProcess>,
}

impl SingleProcessBackend {
    fn new(process: Arc<TestProcess>) -> Self {
        Self { process }
    }
}

impl ExecBackend for SingleProcessBackend {
    fn start(&self, _params: ExecParams) -> ExecBackendFuture<'_> {
        Box::pin(async move {
            Ok(StartedExecProcess {
                process: self.process.clone(),
            })
        })
    }
}

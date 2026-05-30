use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Output;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE;
use chrono::Utc;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use sha2::Digest;
use tempfile::TempDir;
use walkdir::WalkDir;

const TODO_EXPECTED_TOTAL: usize = 21;

#[test]
fn workflow_01_file_stats_live_e2e() -> Result<()> {
    let e2e = WorkflowSelfE2e::new("file-stats")?;
    let before_global = e2e.prepare(FileStatsFixture)?;
    let workflow_dir = e2e.develop_workflow(
        "file-stats",
        "file-stats",
        "Count text files, lines, and extensions in the current repository.",
    )?;
    e2e.implement_workflow(&workflow_dir, "file-stats", file_stats_prompt())?;
    e2e.validate_workflow("file-stats")?;
    assert_generated_tests_replaced(&workflow_dir)?;

    assert_eq!(
        e2e.run_workflow_json("file-stats", RunInput::Default)?,
        json!({
            "totalFiles": 6,
            "totalLines": 16,
            "byExtension": {
                ".json": { "files": 1, "lines": 3 },
                ".md": { "files": 2, "lines": 6 },
                ".rs": { "files": 1, "lines": 3 },
                ".sh": { "files": 1, "lines": 2 },
                ".ts": { "files": 1, "lines": 2 }
            },
            "files": [
                { "path": "README.md", "extension": ".md", "lines": 3 },
                { "path": "data/config.json", "extension": ".json", "lines": 3 },
                { "path": "docs/guide.md", "extension": ".md", "lines": 3 },
                { "path": "scripts/run.sh", "extension": ".sh", "lines": 2 },
                { "path": "src/app.ts", "extension": ".ts", "lines": 2 },
                { "path": "src/lib.rs", "extension": ".rs", "lines": 3 }
            ],
            "summaryMarkdown": "Scanned 6 files with 16 total lines."
        })
    );
    assert_eq!(
        e2e.run_workflow_json(
            "file-stats",
            RunInput::CliFlags(vec![("--extension", "md")]),
        )?,
        json!({
            "totalFiles": 2,
            "totalLines": 6,
            "byExtension": {
                ".md": { "files": 2, "lines": 6 }
            },
            "files": [
                { "path": "README.md", "extension": ".md", "lines": 3 },
                { "path": "docs/guide.md", "extension": ".md", "lines": 3 }
            ],
            "summaryMarkdown": "Scanned 2 files with 6 total lines."
        })
    );

    e2e.assert_invalid_workflow_gate("file-stats", &workflow_dir)?;
    e2e.assert_no_auth_token_leaked()?;
    e2e.assert_global_workflows_unchanged(before_global)?;
    Ok(())
}

#[test]
fn workflow_02_todo_sweep_live_e2e() -> Result<()> {
    let e2e = WorkflowSelfE2e::new("todo-sweep")?;
    let before_global = e2e.prepare(TodoSweepFixture)?;
    let workflow_dir = e2e.develop_workflow(
        "todo-sweep",
        "todo-sweep",
        "Find TODO, FIXME, and XXX markers in the current repository and return total, byTag, items, and summaryMarkdown.",
    )?;
    e2e.implement_workflow(&workflow_dir, "todo-sweep", todo_sweep_prompt())?;
    TodoSweepFixture.add_runtime_cases(&e2e.fixture_repo)?;
    let outside_before_run = snapshot_fixture_outside_workflow(&e2e.fixture_repo, &workflow_dir)?;

    e2e.validate_workflow("todo-sweep")?;
    assert_generated_tests_replaced(&workflow_dir)?;
    assert_todo_sweep_output(
        e2e.run_workflow_json("todo-sweep", RunInput::Default)?,
        TODO_EXPECTED_TOTAL,
        RequiredTodoItems::Present,
    )?;
    assert_todo_sweep_output(
        e2e.run_workflow_json("todo-sweep", RunInput::Json(r#"{"maxItems":10}"#))?,
        10,
        RequiredTodoItems::Skip,
    )?;
    assert_todo_sweep_output(
        e2e.run_workflow_json("todo-sweep", RunInput::CliFlags(vec![("--max-items", "1")]))?,
        1,
        RequiredTodoItems::Skip,
    )?;
    assert_eq!(
        snapshot_fixture_outside_workflow(&e2e.fixture_repo, &workflow_dir)?,
        outside_before_run,
        "fixture files outside the workflow changed during validation and run checks"
    );

    e2e.assert_invalid_workflow_gate("todo-sweep", &workflow_dir)?;
    e2e.assert_no_auth_token_leaked()?;
    e2e.assert_global_workflows_unchanged(before_global)?;
    Ok(())
}

#[test]
fn workflow_03_release_audit_live_e2e() -> Result<()> {
    let e2e = WorkflowSelfE2e::new("release-audit")?;
    let before_global = e2e.prepare(ReleaseAuditFixture)?;
    let workflow_dir = e2e.develop_workflow(
        "release-audit",
        "release-audit",
        "Audit release readiness from package metadata, changelog, tests, docs, and git state.",
    )?;
    e2e.implement_workflow(&workflow_dir, "release-audit", release_audit_prompt())?;
    let outside_before_run = snapshot_fixture_outside_workflow(&e2e.fixture_repo, &workflow_dir)?;

    e2e.validate_workflow("release-audit")?;
    assert_generated_tests_replaced(&workflow_dir)?;
    assert_eq!(
        e2e.run_workflow_json("release-audit", RunInput::Default)?,
        json!({
            "releaseVersion": "2.0.0",
            "status": "blocked",
            "counts": { "blocking": 2, "warning": 1, "pass": 2 },
            "checks": [
                {
                    "id": "package-version",
                    "level": "pass",
                    "source": "package.json",
                    "message": "package.json declares version 2.0.0"
                },
                {
                    "id": "changelog-entry",
                    "level": "blocking",
                    "source": "CHANGELOG.md",
                    "message": "CHANGELOG.md is missing a 2.0.0 section"
                },
                {
                    "id": "test-script",
                    "level": "pass",
                    "source": "package.json",
                    "message": "package.json defines a test script"
                },
                {
                    "id": "docs-version",
                    "level": "warning",
                    "source": "docs/release.md",
                    "message": "docs/release.md does not mention 2.0.0"
                },
                {
                    "id": "git-clean",
                    "level": "blocking",
                    "source": "git",
                    "message": "working tree has uncommitted changes outside .codex"
                }
            ],
            "summaryMarkdown": "Release 2.0.0 is blocked: 2 blocking checks, 1 warning, 2 passing checks."
        })
    );
    assert_eq!(
        snapshot_fixture_outside_workflow(&e2e.fixture_repo, &workflow_dir)?,
        outside_before_run,
        "fixture files outside the workflow changed during validation and run checks"
    );

    e2e.assert_invalid_workflow_gate("release-audit", &workflow_dir)?;
    e2e.assert_no_auth_token_leaked()?;
    e2e.assert_global_workflows_unchanged(before_global)?;
    Ok(())
}

#[test]
fn workflow_04_code_review_readme_live_e2e() -> Result<()> {
    let e2e = WorkflowSelfE2e::new("code-review")?;
    let code_review_readme = e2e.read_real_code_review_readme()?;
    let before_global = e2e.prepare(CodeReviewFixture)?;
    let workflow_dir = e2e.develop_workflow(
        "code-review",
        "code-review",
        "Run a code review and read stored review reports.",
    )?;
    e2e.implement_workflow(
        &workflow_dir,
        "code-review",
        code_review_prompt(&code_review_readme),
    )?;
    let outside_before_run = snapshot_fixture_outside_workflow(&e2e.fixture_repo, &workflow_dir)?;

    e2e.validate_workflow("code-review")?;
    assert_generated_tests_replaced(&workflow_dir)?;

    let database_path = e2e.temp.path().join("code-review-state.sqlite3");
    let artifacts_dir = e2e.temp.path().join("code-review-artifacts");
    let review_output = e2e.run_workflow_json(
        "code-review",
        RunInput::JsonOwned(json!({
            "workingDirectory": path_str(&e2e.fixture_repo),
            "targetRef": "HEAD",
            "baseRef": "main",
            "scope": "branch",
            "limit": 5,
            "applyFixes": false,
            "databasePath": path_str(&database_path),
            "artifactsDir": path_str(&artifacts_dir),
            "output": "json"
        })),
    )?;
    let review_id = review_output
        .get("reviewId")
        .and_then(Value::as_str)
        .or_else(|| {
            review_output
                .get("report")
                .and_then(|report| report.get("reviewId"))
                .and_then(Value::as_str)
        })
        .context("code-review output must include reviewId")?
        .to_string();
    let report = review_output.get("report").unwrap_or(&review_output);
    assert_code_review_found_auth_bug(report)?;

    let read_report_output = e2e.run_workflow_json(
        "code-review",
        RunInput::JsonOwned(json!({
            "action": "read-report",
            "reviewId": review_id,
            "databasePath": path_str(&database_path),
            "artifactsDir": path_str(&artifacts_dir),
            "output": "md"
        })),
    )?;
    let markdown = read_report_output
        .get("markdown")
        .and_then(Value::as_str)
        .context("read-report output must include markdown")?;
    assert!(
        markdown.trim_start().starts_with("#"),
        "read-report markdown should be non-empty markdown: {read_report_output:#}"
    );
    assert!(
        markdown.contains("auth") || markdown.contains("admin") || markdown.contains("bypass"),
        "read-report markdown should mention the known defect: {markdown}"
    );
    assert!(
        read_report_output.get("report").is_some(),
        "read-report should return the stored report: {read_report_output:#}"
    );
    assert_eq!(
        snapshot_fixture_outside_workflow(&e2e.fixture_repo, &workflow_dir)?,
        outside_before_run,
        "fixture files outside the workflow changed during validation and run checks"
    );

    e2e.assert_invalid_workflow_gate("code-review", &workflow_dir)?;
    e2e.assert_no_auth_token_leaked()?;
    e2e.assert_global_workflows_unchanged(before_global)?;
    Ok(())
}

struct WorkflowSelfE2e {
    temp: TempDir,
    codex_home: PathBuf,
    fixture_repo: PathBuf,
    real_codex_home: PathBuf,
    codex_bin: PathBuf,
}

impl WorkflowSelfE2e {
    fn new(test_name: &str) -> Result<Self> {
        let temp = tempfile::Builder::new()
            .prefix(&format!("codex-workflow-e2e-{test_name}-"))
            .tempdir()?;
        let codex_home = temp.path().join("codex-home");
        let fixture_repo = temp.path().join("fixture-repo");
        let real_codex_home = real_codex_home()?;
        let codex_bin = codex_utils_cargo_bin::cargo_bin("codex")?;
        Ok(Self {
            temp,
            codex_home,
            fixture_repo,
            real_codex_home,
            codex_bin,
        })
    }

    fn prepare(&self, fixture: impl WorkflowFixture) -> Result<Vec<String>> {
        fs::create_dir_all(&self.codex_home)?;
        self.seed_managed_bun_runtime()?;
        self.seed_real_world_auth()?;
        self.assert_real_world_auth_is_isolated()?;
        fixture.write(&self.fixture_repo)?;
        self.write_config()?;
        snapshot_global_workflows(&self.real_codex_home)
    }

    fn develop_workflow(&self, id: &str, command: &str, description: &str) -> Result<PathBuf> {
        self.assert_codex_success(
            self.run_codex(vec![
                "-C".to_string(),
                path_str(&self.fixture_repo),
                "workflow".to_string(),
                "develop".to_string(),
                "--location".to_string(),
                "project".to_string(),
                "--id".to_string(),
                id.to_string(),
                "--command".to_string(),
                command.to_string(),
                description.to_string(),
            ])?,
            "workflow develop",
        )?;

        let output = self.run_codex(vec![
            "-C".to_string(),
            path_str(&self.fixture_repo),
            "workflow".to_string(),
            "where".to_string(),
            id.to_string(),
        ])?;
        if !output.status.success() {
            self.assert_codex_success(output, "workflow where")?;
            unreachable!("assert_codex_success returns on successful status only");
        }
        let workflow_dir = PathBuf::from(String::from_utf8(output.stdout)?.trim());
        let expected = self.fixture_repo.join(format!(".codex/workflows/{id}"));
        assert_eq!(workflow_dir, expected);
        Ok(workflow_dir)
    }

    fn implement_workflow(
        &self,
        workflow_dir: &Path,
        workflow_id: &str,
        prompt: String,
    ) -> Result<()> {
        let outside_after_scaffold =
            snapshot_fixture_outside_workflow(&self.fixture_repo, workflow_dir)?;
        let full_prompt = format!(
            "{}\n\n{}\n\nWorkflow id: `{}`\nWorkflow directory: `{}`\nFixture repository root: `{}`\n",
            live_implementation_constraints(workflow_id),
            prompt,
            workflow_id,
            workflow_dir.display(),
            self.fixture_repo.display(),
        );
        let output = self.run_codex(vec![
            "exec".to_string(),
            "-C".to_string(),
            path_str(workflow_dir),
            "--sandbox".to_string(),
            "workspace-write".to_string(),
            "--skip-git-repo-check".to_string(),
            full_prompt,
        ])?;
        self.assert_no_auth_token_leaked()?;
        self.assert_codex_success(output, "codex exec workflow implementation")?;
        assert_eq!(
            snapshot_fixture_outside_workflow(&self.fixture_repo, workflow_dir)?,
            outside_after_scaffold,
            "fixture files outside the workflow changed during implementation"
        );
        Ok(())
    }

    fn validate_workflow(&self, id: &str) -> Result<()> {
        let output = self.run_codex(vec![
            "-C".to_string(),
            path_str(&self.fixture_repo),
            "workflow".to_string(),
            "validate".to_string(),
            id.to_string(),
        ])?;
        if !output.status.success() || output.stdout != b"valid\n" {
            bail!(
                "validation output was not exactly valid:\nstdout:\n{}\nstderr:\n{}",
                self.sanitize(&String::from_utf8_lossy(&output.stdout)),
                self.sanitize(&String::from_utf8_lossy(&output.stderr))
            );
        }
        Ok(())
    }

    fn run_workflow_json(&self, id: &str, input: RunInput<'_>) -> Result<Value> {
        let mut args = vec![
            "-C".to_string(),
            path_str(&self.fixture_repo),
            "workflow".to_string(),
            "run".to_string(),
            id.to_string(),
        ];
        match input {
            RunInput::Default => {}
            RunInput::Json(input_json) => {
                args.push("--input".to_string());
                args.push(input_json.to_string());
            }
            RunInput::JsonOwned(input_json) => {
                args.push("--input".to_string());
                args.push(serde_json::to_string(&input_json)?);
            }
            RunInput::CliFlags(flags) => {
                for (flag, value) in flags {
                    args.push(flag.to_string());
                    args.push(value.to_string());
                }
            }
        }

        let output = self.run_codex(args)?;
        if !output.status.success() {
            self.assert_codex_success(output, "workflow run")?;
            unreachable!("assert_codex_success returns on successful status only");
        }
        serde_json::from_slice(&output.stdout)
            .with_context(|| format!("workflow {id} did not print JSON to stdout"))
    }

    fn assert_invalid_workflow_gate(&self, id: &str, workflow_dir: &Path) -> Result<()> {
        let workflow_yaml = workflow_dir.join("workflow.yaml");
        let valid = fs::read_to_string(&workflow_yaml)?;
        let corrupted = corrupt_workflow_id(&valid, &format!("{id}-corrupted"))?;
        fs::write(&workflow_yaml, corrupted)?;

        let validate_output = self.run_codex(vec![
            "-C".to_string(),
            path_str(&self.fixture_repo),
            "workflow".to_string(),
            "validate".to_string(),
            id.to_string(),
        ])?;
        let run_output = self.run_codex(vec![
            "-C".to_string(),
            path_str(&self.fixture_repo),
            "workflow".to_string(),
            "run".to_string(),
            id.to_string(),
        ])?;
        fs::write(&workflow_yaml, valid)?;

        let validate_text = combined_output(&validate_output);
        if validate_output.status.success() {
            bail!(
                "corrupted workflow unexpectedly validated successfully:\n{}",
                self.sanitize(&validate_text)
            );
        }
        if validate_text.lines().any(|line| line == "valid") {
            bail!(
                "corrupted workflow validation printed a standalone valid line:\n{}",
                self.sanitize(&validate_text)
            );
        }

        let run_text = combined_output(&run_output);
        if run_output.status.success() {
            bail!(
                "corrupted workflow unexpectedly ran successfully:\n{}",
                self.sanitize(&run_text)
            );
        }
        if !run_text.contains("invalid and cannot be run") {
            bail!(
                "corrupted workflow did not report that invalid workflows cannot run:\n{}",
                self.sanitize(&run_text)
            );
        }
        Ok(())
    }

    fn read_real_code_review_readme(&self) -> Result<String> {
        let path = self.real_codex_home.join("workflows/code-review/README.md");
        fs::read_to_string(&path).with_context(|| {
            format!(
                "failed to read real code-review README at {}",
                path.display()
            )
        })
    }

    fn write_config(&self) -> Result<()> {
        let model = std::env::var("CODEX_WORKFLOW_SELF_E2E_MODEL")
            .unwrap_or_else(|_| "gpt-5.2".to_string());
        let config = format!(
            r#"model = "{}"
approval_policy = "never"
suppress_unstable_features_warning = true

[features]
workflows = true

[analytics]
enabled = false

[workflows]
default_location = "project"
commit_policy = "manual"
dependency_update_policy = "manual"
repair_mode = "full"

[projects."{}"]
trust_level = "trusted"
"#,
            toml_escape(&model),
            toml_escape(&path_str(&self.fixture_repo)),
        );
        fs::write(self.codex_home.join("config.toml"), config)?;
        Ok(())
    }

    fn seed_managed_bun_runtime(&self) -> Result<()> {
        let source_bin = self.real_codex_home.join("workflows/.bin");
        let target_bin = self.codex_home.join("workflows/.bin");
        for file_name in ["bun", ".bun-version"] {
            let source = source_bin.join(file_name);
            if source.is_file() {
                fs::create_dir_all(&target_bin)?;
                fs::copy(source, target_bin.join(file_name))?;
            }
        }
        Ok(())
    }

    fn seed_real_world_auth(&self) -> Result<()> {
        if let Some(api_key) = optional_env_secret("OPENAI_API_KEY")
            .or_else(|| config_provider_token(&self.real_codex_home, "openai"))
        {
            return write_api_key_auth(&self.codex_home.join("auth.json"), &api_key);
        }

        match self.try_seed_chatgpt_auth()? {
            AuthSeedResult::Seeded => Ok(()),
            AuthSeedResult::OnlyStaleTokens(labels) => {
                bail!(
                    "found only expired or near-expiry Codex access tokens: {}",
                    labels.join(", ")
                );
            }
            AuthSeedResult::NoUsableAuth => {
                bail!(
                    "no OPENAI_API_KEY, [model_providers.openai].token, or usable ChatGPT access token found in current Codex home"
                );
            }
        }
    }

    fn try_seed_chatgpt_auth(&self) -> Result<AuthSeedResult> {
        let auth_path = self.codex_home.join("auth.json");
        let mut stale_tokens = Vec::new();
        for (label, path) in auth_candidates(&self.real_codex_home)? {
            let Some(data) = read_json_file(&path)? else {
                continue;
            };
            if let Some(api_key) = data
                .get("OPENAI_API_KEY")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
            {
                write_api_key_auth(&auth_path, api_key)?;
                return Ok(AuthSeedResult::Seeded);
            }

            let Some(tokens) = data.get("tokens").and_then(Value::as_object) else {
                continue;
            };
            let Some(access_token) = tokens
                .get("access_token")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
            else {
                continue;
            };
            match token_expires_at(access_token) {
                Ok(expires_at) if expires_at > Utc::now().timestamp() + 300 => {
                    write_chatgpt_token_auth(&auth_path, tokens)?;
                    return Ok(AuthSeedResult::Seeded);
                }
                Ok(_) => stale_tokens.push(label),
                Err(err) => {
                    eprintln!(
                        "skipping {label} because its access token expiry could not be inspected: {err}"
                    );
                }
            }
        }

        if stale_tokens.is_empty() {
            Ok(AuthSeedResult::NoUsableAuth)
        } else {
            Ok(AuthSeedResult::OnlyStaleTokens(stale_tokens))
        }
    }

    fn assert_real_world_auth_is_isolated(&self) -> Result<()> {
        let auth = fs::read_to_string(self.codex_home.join("auth.json"))
            .context("failed to read isolated auth file")?;
        let auth: Value = serde_json::from_str(&auth)?;
        if auth["tokens"]["refresh_token"]
            .as_str()
            .is_some_and(|token| !token.trim().is_empty())
        {
            bail!("isolated real-world auth retained a reusable refresh token");
        }
        if auth["OPENAI_API_KEY"]
            .as_str()
            .is_some_and(|api_key| !api_key.trim().is_empty())
        {
            return Ok(());
        }
        let Some(access_token) = auth["tokens"]["access_token"]
            .as_str()
            .filter(|token| !token.trim().is_empty())
        else {
            bail!("isolated real-world auth has neither an API key nor a ChatGPT access token");
        };
        if token_expires_at(access_token)? <= Utc::now().timestamp() + 300 {
            bail!("isolated ChatGPT access token expires in less than five minutes");
        }
        Ok(())
    }

    fn assert_no_auth_token_leaked(&self) -> Result<()> {
        let auth_path = self.codex_home.join("auth.json");
        let secrets = auth_secrets(&auth_path)?;
        if secrets.is_empty() {
            return Ok(());
        }
        for entry in WalkDir::new(self.temp.path())
            .into_iter()
            .filter_map(std::result::Result::ok)
        {
            let path = entry.path();
            if !path.is_file() || path == auth_path {
                continue;
            }
            let Ok(contents) = fs::read(path) else {
                continue;
            };
            for (label, secret) in &secrets {
                if contents
                    .windows(secret.len())
                    .any(|window| window == secret.as_slice())
                {
                    bail!(
                        "{label} leaked into {}",
                        path.strip_prefix(self.temp.path())?.display()
                    );
                }
            }
        }
        Ok(())
    }

    fn assert_global_workflows_unchanged(&self, before_global: Vec<String>) -> Result<()> {
        let after_global = snapshot_global_workflows(&self.real_codex_home)?;
        assert_eq!(
            after_global, before_global,
            "real global workflow directory changed during isolated e2e"
        );
        Ok(())
    }

    fn run_codex<I, S>(&self, args: I) -> Result<Output>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut command = Command::new(&self.codex_bin);
        command
            .env("CODEX_HOME", &self.codex_home)
            .env("CODEX_SQLITE_HOME", &self.codex_home)
            .args(args);
        Ok(command.output()?)
    }

    fn assert_codex_success(&self, output: Output, context: &str) -> Result<()> {
        if !output.status.success() {
            bail!(
                "{context} failed with status {}:\nstdout:\n{}\nstderr:\n{}",
                output.status,
                self.sanitize(&String::from_utf8_lossy(&output.stdout)),
                self.sanitize(&String::from_utf8_lossy(&output.stderr))
            );
        }
        Ok(())
    }

    fn sanitize(&self, text: &str) -> String {
        let mut sanitized = text.to_string();
        if let Ok(secrets) = auth_secrets(&self.codex_home.join("auth.json")) {
            for (_, secret) in secrets {
                if let Ok(secret) = String::from_utf8(secret) {
                    sanitized = sanitized.replace(&secret, "[REDACTED_AUTH_TOKEN]");
                }
            }
        }
        sanitized
    }
}

trait WorkflowFixture {
    fn write(&self, root: &Path) -> Result<()>;
}

struct FileStatsFixture;

impl WorkflowFixture for FileStatsFixture {
    fn write(&self, root: &Path) -> Result<()> {
        for dir in [
            ".codex",
            "artifacts",
            "data",
            "docs",
            "node_modules/pkg",
            "scripts",
            "src",
            "state",
        ] {
            fs::create_dir_all(root.join(dir))?;
        }
        for (relative, contents) in [
            ("README.md", "# File stats fixture\n\nTracks files.\n"),
            ("src/lib.rs", "pub fn alpha() {}\n\npub fn beta() {}\n"),
            ("src/app.ts", "const a = 1;\nconst b = 2;\n"),
            ("docs/guide.md", "# Guide\nDetails here.\nMore details.\n"),
            ("scripts/run.sh", "#!/usr/bin/env bash\necho run\n"),
            ("data/config.json", "{\n  \"enabled\": true\n}\n"),
            (".codex/ignored.md", "ignored\n"),
            ("node_modules/pkg/index.js", "console.log('ignored');\n"),
            ("artifacts/out.txt", "ignored\n"),
            ("state/cache.txt", "ignored\n"),
        ] {
            fs::write(root.join(relative), contents)?;
        }
        fs::write(root.join("src/blob.bin"), [0, 159, 146, 150, 0, 1, 2, 3])?;
        init_git_repo(root)?;
        fs::write(root.join(".git/ignored-file.txt"), "ignored\n")?;
        Ok(())
    }
}

struct TodoSweepFixture;

impl WorkflowFixture for TodoSweepFixture {
    fn write(&self, root: &Path) -> Result<()> {
        for dir in [
            ".codex",
            "artifacts",
            "data",
            "docs",
            "node_modules/ignored-package",
            "scripts",
            "src/nested/deeper",
            "state",
        ] {
            fs::create_dir_all(root.join(dir))?;
        }
        for (relative, contents) in [
            (
                "README.md",
                "# Fixture Repository\n\nThis repository intentionally contains markers for the todo-sweep workflow.\n",
            ),
            (
                "src/main.ts",
                "// TODO: tighten config parsing.\n// FIXME: preserve user edits during rewrite.\n// XXX: document retry policy.\nexport const main = 1;\n",
            ),
            (
                "src/nested/worker.ts",
                "// TODO: handle batch retries.\n// TODO: normalize Windows paths.\n// FIXME: avoid duplicate diagnostics.\n// XXX: investigate parallel scan ordering.\nexport const worker = 2;\n",
            ),
            (
                "src/nested/deeper/feature.ts",
                "// TODO: cover release notes.\n// FIXME: validate CLI options.\n// XXX: remove temporary adapter.\nexport const feature = 3;\n",
            ),
            (
                "TASKS",
                "TODO: follow up on extensionless files.\nFIXME: extensionless marker should count.\n",
            ),
            (
                "docs/release.md",
                "# Release Notes\n\nXXX: confirm release checklist.\nTODO: update migration notes.\n",
            ),
            (
                "src/mixed.txt",
                "One line can include TODO: first inline marker and FIXME: second inline marker.\n",
            ),
            (
                "data/config.yaml",
                "settings:\n  # FIXME: config comment markers should count.\n",
            ),
            (
                "docs/notes.md",
                "# Notes\n\nThis file intentionally has no counted task markers.\n",
            ),
            ("scripts/ops.sh", "#!/usr/bin/env bash\necho ok\n"),
            (
                ".codex/ignored.ts",
                "// TODO: ignored Codex metadata marker.\n",
            ),
            (
                "node_modules/ignored-package/index.js",
                "// FIXME: ignored dependency marker.\n",
            ),
            ("artifacts/report.txt", "XXX: ignored artifact marker.\n"),
            ("state/cache.txt", "TODO: ignored state marker.\n"),
        ] {
            fs::write(root.join(relative), contents)?;
        }
        fs::write(root.join("src/binary.dat"), [0, 159, 146, 150, 0, 1, 2, 3])?;
        init_git_repo(root)?;
        fs::write(
            root.join(".git/ignored-marker.txt"),
            "TODO: ignored git metadata marker.\n",
        )?;
        Ok(())
    }
}

impl TodoSweepFixture {
    fn add_runtime_cases(&self, root: &Path) -> Result<()> {
        fs::create_dir_all(root.join("late"))?;
        fs::write(
            root.join("late/after-implementation.md"),
            "TODO: detect files added after implementation.\nFIXME: keep counts dynamic.\n",
        )?;
        fs::write(
            root.join("late/compound.txt"),
            "XXX: first late inline marker TODO: second late inline marker\n",
        )?;
        fs::write(
            root.join("node_modules/ignored-package/late.js"),
            "// TODO: ignored late dependency marker.\n",
        )?;
        fs::write(
            root.join("state/late-cache.txt"),
            "FIXME: ignored late state marker.\n",
        )?;
        Ok(())
    }
}

struct ReleaseAuditFixture;

impl WorkflowFixture for ReleaseAuditFixture {
    fn write(&self, root: &Path) -> Result<()> {
        for dir in ["docs", "src", "test"] {
            fs::create_dir_all(root.join(dir))?;
        }
        fs::write(
            root.join("package.json"),
            r#"{
  "name": "release-audit-fixture",
  "version": "2.0.0",
  "scripts": {
    "build": "tsc -p tsconfig.json",
    "test": "node test/run.test.js"
  }
}
"#,
        )?;
        fs::write(
            root.join("CHANGELOG.md"),
            "# Changelog\n\n## 1.9.0\n\n- Previous release.\n",
        )?;
        fs::write(
            root.join("docs/release.md"),
            "# Release Process\n\nCurrent docs cover version 1.9.0 only.\n",
        )?;
        fs::write(
            root.join("test/run.test.js"),
            "console.log('tests pass');\n",
        )?;
        fs::write(
            root.join("src/index.js"),
            "export const version = '2.0.0';\n",
        )?;
        init_git_repo(root)?;
        fs::write(
            root.join("src/index.js"),
            "export const version = '2.0.0';\nexport const dirty = true;\n",
        )?;
        Ok(())
    }
}

struct CodeReviewFixture;

impl WorkflowFixture for CodeReviewFixture {
    fn write(&self, root: &Path) -> Result<()> {
        fs::create_dir_all(root.join("src"))?;
        fs::write(
            root.join("package.json"),
            r#"{
  "name": "code-review-fixture",
  "version": "1.0.0",
  "type": "module",
  "scripts": {
    "test": "node --test"
  }
}
"#,
        )?;
        fs::write(
            root.join("src/auth.ts"),
            "export function isAdmin(user: { role: string }, password: string): boolean {\n  return user.role === \"admin\" && password.length > 12;\n}\n",
        )?;
        init_git_repo(root)?;
        assert_process_success(
            Command::new("git")
                .arg("-C")
                .arg(root)
                .arg("checkout")
                .arg("-b")
                .arg("feature/auth-bug")
                .output()?,
            "git checkout -b feature/auth-bug",
        )?;
        fs::write(
            root.join("src/auth.ts"),
            "export function isAdmin(user: { role: string }, password: string): boolean {\n  if (password === \"admin\") {\n    return true;\n  }\n  return user.role === \"admin\";\n}\n",
        )?;
        assert_process_success(
            Command::new("git")
                .arg("-C")
                .arg(root)
                .arg("add")
                .arg(".")
                .output()?,
            "git add auth bug",
        )?;
        assert_process_success(
            Command::new("git")
                .arg("-C")
                .arg(root)
                .arg("-c")
                .arg("user.name=Codex")
                .arg("-c")
                .arg("user.email=codex@openai.com")
                .arg("commit")
                .arg("-m")
                .arg("introduce auth bug")
                .output()?,
            "git commit auth bug",
        )?;
        Ok(())
    }
}

#[derive(Debug)]
enum RunInput<'a> {
    Default,
    Json(&'a str),
    JsonOwned(Value),
    CliFlags(Vec<(&'a str, &'a str)>),
}

#[derive(Debug, Copy, Clone)]
enum RequiredTodoItems {
    Present,
    Skip,
}

#[derive(Debug, Copy, Clone)]
struct ExpectedTodoItem {
    tag: &'static str,
    file: &'static str,
    line: u64,
    text_contains: &'static str,
}

enum AuthSeedResult {
    Seeded,
    OnlyStaleTokens(Vec<String>),
    NoUsableAuth,
}

fn file_stats_prompt() -> String {
    r#"Implement the file-stats workflow in this workflow directory only.

Runtime contract:
- Traverse ctx.cwd recursively and inspect regular text files.
- Skip .codex, .git, node_modules, artifacts, and state directories recursively.
- Skip binary/unreadable files.
- Count lines as newline-separated text lines, ignoring a final trailing empty split caused only by a terminal newline.
- Compute extension from the file name, including the leading dot. Files without an extension use "(none)".
- Sort files by normalized slash-separated relative path.
- Accept optional input.extension and CLI --extension. The value may be "md" or ".md"; normalize it to ".md".
- Filtering applies before totals and byExtension are computed.

Return exactly this JSON shape and no extra fields:
{
  "totalFiles": number,
  "totalLines": number,
  "byExtension": { "<extension>": { "files": number, "lines": number } },
  "files": [{ "path": string, "extension": string, "lines": number }],
  "summaryMarkdown": string
}

For the provided fixture, the default run must return exactly:
- totalFiles 6 and totalLines 16
- byExtension .json 1/3, .md 2/6, .rs 1/3, .sh 1/2, .ts 1/2
- files README.md, data/config.json, docs/guide.md, scripts/run.sh, src/app.ts, src/lib.rs with their exact line counts
- summaryMarkdown "Scanned 6 files with 16 total lines."

For CLI input --extension md, return only README.md and docs/guide.md with summaryMarkdown "Scanned 2 files with 6 total lines.""#.to_string()
}

fn todo_sweep_prompt() -> String {
    r#"Implement the todo-sweep workflow in this workflow directory only.

Runtime contract:
- Scan every regular text file under ctx.cwd for TODO, FIXME, and XXX markers without filtering by file extension.
- Skip .codex, .git, node_modules, artifacts, and state directories recursively.
- Honor input maxItems and CLI --max-items by limiting only the returned items array after computing all matches.
- total and byTag must always describe every marker found before truncation.
- Return JSON with exactly the top-level fields total, byTag, items, and summaryMarkdown.
- Use byTag keys TODO, FIXME, and XXX.
- Use item fields tag, file, line, and text.
- Count every marker occurrence, including multiple markers on the same line as separate items.
- For same-line markers, item text must stop before the next marker on that line.
- Sort items by file path, then line number, then marker position.
- The e2e adds more files after implementation before running the workflow. Do not hard-code paths or counts from the initial fixture.
- Do not restrict scanning to a hard-coded extension allowlist; traverse regular text files and skip binary files.
- Also support CLI input fields, for example codex workflow run todo-sweep --max-items 1.

For the complete fixture after late files are added, the workflow must report total 21 and byTag TODO 9, FIXME 7, XXX 5."#.to_string()
}

fn release_audit_prompt() -> String {
    r#"Implement the release-audit workflow in this workflow directory only.

Runtime contract:
- Read package.json, CHANGELOG.md, docs/release.md, test files, and git status from ctx.cwd.
- Determine releaseVersion from input.releaseVersion when present, otherwise from package.json version.
- Ignore .codex paths when evaluating git cleanliness because the workflow under test lives there.
- Do not run npm install or fetch dependencies.
- Return exactly this JSON shape and no extra fields:
{
  "releaseVersion": string,
  "status": "ready" | "blocked",
  "counts": { "blocking": number, "warning": number, "pass": number },
  "checks": [{ "id": string, "level": "pass" | "warning" | "blocking", "source": string, "message": string }],
  "summaryMarkdown": string
}

For the provided fixture, the default run must return exactly:
{
  "releaseVersion": "2.0.0",
  "status": "blocked",
  "counts": { "blocking": 2, "warning": 1, "pass": 2 },
  "checks": [
    { "id": "package-version", "level": "pass", "source": "package.json", "message": "package.json declares version 2.0.0" },
    { "id": "changelog-entry", "level": "blocking", "source": "CHANGELOG.md", "message": "CHANGELOG.md is missing a 2.0.0 section" },
    { "id": "test-script", "level": "pass", "source": "package.json", "message": "package.json defines a test script" },
    { "id": "docs-version", "level": "warning", "source": "docs/release.md", "message": "docs/release.md does not mention 2.0.0" },
    { "id": "git-clean", "level": "blocking", "source": "git", "message": "working tree has uncommitted changes outside .codex" }
  ],
  "summaryMarkdown": "Release 2.0.0 is blocked: 2 blocking checks, 1 warning, 2 passing checks."
}"#.to_string()
}

fn code_review_prompt(readme: &str) -> String {
    format!(
        r#"Implement the code-review workflow in this workflow directory only using the README contract below as the product contract.

For this live e2e, implement a compact deterministic local reviewer. Do not spawn Codex agents from the generated workflow, do not apply fixes, and do not require network access during workflow run. The live Codex call is only for implementing this workflow package.

Required runtime behavior:
- Support action "review" by default and action "read-report".
- For review, inspect the git diff from merge-base(baseRef, targetRef)..targetRef in input. Use ctx.cwd or input.workingDirectory as the repository root.
- Detect the known security defect in the fixture: src/auth.ts adds a branch where password === "admin" returns true. Emit at least one finding whose serialized JSON mentions src/auth.ts and mentions admin, auth, or bypass.
- Return a reviewId and a report object. The report must include findings as an array either at report.findings or top-level findings, and counts.initialFindings must be at least 1 when the defect is present.
- Persist the report under input.databasePath so action "read-report" can load it later in a separate workflow run. A SQLite implementation is preferred, but a durable local file at that path is acceptable for this e2e if the external contract is preserved.
- For action "read-report", require reviewId, load the stored report, and return {{ "reviewId": string, "markdown": string, "report": object }}. The markdown must be non-empty markdown and mention the known defect.
- Support input.output "json" and "md". "md" should still return a JSON object containing markdown.
- Validate through codex workflow validate code-review with stdout exactly valid.

Real code-review README follows:

```md
{readme}
```"#
    )
}

fn live_implementation_constraints(workflow_id: &str) -> String {
    format!(
        r#"Real workflow e2e constraints:
- Replace the scaffold implementation with real workflow logic for {workflow_id}.
- Replace every scaffold placeholder test. The load test must do more than export {{}}; autocomplete must assert a non-empty useful suggestion; the positive test must assert real workflow output, not {{ ok: true, input }}.
- If you import node:fs, node:fs/promises, node:path, node:child_process, node:crypto, node:os, node:url, or other Node/Bun built-ins, add src/types.d.ts with declarations for the exact APIs you use and put the triple-slash reference path directive at the top of src/workflow.ts.
- Do not edit files outside this workflow directory.
- Do not write generated artifacts inside this workflow directory from tests. Use temporary directories under /tmp for test fixture files.
- Keep package.json dependency versions pinned; do not use latest or *.
- Do not rely on globally installed third-party packages. Prefer built-in platform APIs.
- If you call ctx.status, always pass an object with non-empty workflowName and workflowStatus fields, for example ctx.status({{ workflowName: "{workflow_id}", workflowStatus: "running" }}). ctx.progress("message", data) is also acceptable.
- Run codex workflow validate {workflow_id} from the fixture repository root until stdout is exactly valid."#
    )
}

fn assert_todo_sweep_output(
    data: Value,
    expected_items: usize,
    required_items: RequiredTodoItems,
) -> Result<()> {
    assert_eq!(
        data.as_object()
            .context("workflow output should be a JSON object")?
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "byTag".to_string(),
            "items".to_string(),
            "summaryMarkdown".to_string(),
            "total".to_string(),
        ])
    );
    assert_eq!(data["total"], json!(TODO_EXPECTED_TOTAL));
    assert_eq!(data["byTag"], json!({"TODO": 9, "FIXME": 7, "XXX": 5}));
    assert!(
        data["summaryMarkdown"]
            .as_str()
            .is_some_and(|summary| !summary.trim().is_empty()),
        "summaryMarkdown must be a non-empty string: {data:#}"
    );
    let items = data["items"]
        .as_array()
        .context("items should be an array")?;
    assert_eq!(items.len(), expected_items);
    for (index, item) in items.iter().enumerate() {
        assert_eq!(
            item.as_object()
                .context("workflow item should be an object")?
                .keys()
                .cloned()
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                "file".to_string(),
                "line".to_string(),
                "tag".to_string(),
                "text".to_string(),
            ])
        );
        let file = item["file"]
            .as_str()
            .with_context(|| format!("item {index} file should be a string"))?;
        let blocked = file
            .replace('\\', "/")
            .split('/')
            .filter(|part| {
                matches!(
                    *part,
                    ".codex" | ".git" | "node_modules" | "artifacts" | "state"
                )
            })
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        assert!(
            blocked.is_empty(),
            "item {index} included ignored path segment(s) {blocked:?} in {file}"
        );
        assert!(
            matches!(item["tag"].as_str(), Some("TODO" | "FIXME" | "XXX")),
            "item {index} tag should be TODO/FIXME/XXX: {item:#}"
        );
        assert!(
            item["line"].as_u64().is_some_and(|line| line > 0),
            "item {index} line should be a positive integer: {item:#}"
        );
        assert!(
            item["text"]
                .as_str()
                .is_some_and(|text| !text.trim().is_empty()),
            "item {index} text should be a non-empty string: {item:#}"
        );
    }

    match required_items {
        RequiredTodoItems::Present => assert_required_todo_items_present(items)?,
        RequiredTodoItems::Skip => {}
    }
    Ok(())
}

fn assert_required_todo_items_present(items: &[Value]) -> Result<()> {
    for expected in [
        ExpectedTodoItem {
            tag: "TODO",
            file: "TASKS",
            line: 1,
            text_contains: "extensionless files",
        },
        ExpectedTodoItem {
            tag: "XXX",
            file: "docs/release.md",
            line: 3,
            text_contains: "release checklist",
        },
        ExpectedTodoItem {
            tag: "TODO",
            file: "src/mixed.txt",
            line: 1,
            text_contains: "first inline marker",
        },
        ExpectedTodoItem {
            tag: "FIXME",
            file: "src/mixed.txt",
            line: 1,
            text_contains: "second inline marker",
        },
        ExpectedTodoItem {
            tag: "TODO",
            file: "late/after-implementation.md",
            line: 1,
            text_contains: "after implementation",
        },
        ExpectedTodoItem {
            tag: "XXX",
            file: "late/compound.txt",
            line: 1,
            text_contains: "first late inline marker",
        },
        ExpectedTodoItem {
            tag: "TODO",
            file: "late/compound.txt",
            line: 1,
            text_contains: "second late inline marker",
        },
    ] {
        let found = items.iter().any(|item| {
            item["tag"].as_str() == Some(expected.tag)
                && item["file"]
                    .as_str()
                    .map(|file| file.replace('\\', "/"))
                    .as_deref()
                    == Some(expected.file)
                && item["line"].as_u64() == Some(expected.line)
                && item["text"]
                    .as_str()
                    .is_some_and(|text| text.contains(expected.text_contains))
        });
        if !found {
            bail!("missing expected workflow item {expected:?} in {items:#?}");
        }
    }
    Ok(())
}

fn assert_code_review_found_auth_bug(report: &Value) -> Result<()> {
    let findings = report
        .get("findings")
        .and_then(Value::as_array)
        .or_else(|| {
            report
                .get("report")
                .and_then(|nested| nested.get("findings"))
                .and_then(Value::as_array)
        })
        .context("code-review report must include findings array")?;
    assert!(
        !findings.is_empty(),
        "code-review should find the known auth bug: {report:#}"
    );
    let has_known_bug = findings.iter().any(|finding| {
        let serialized = finding.to_string().to_ascii_lowercase();
        serialized.contains("src/auth.ts")
            && (serialized.contains("admin")
                || serialized.contains("auth")
                || serialized.contains("bypass"))
    });
    assert!(
        has_known_bug,
        "code-review findings should point at the known src/auth.ts defect: {findings:#?}"
    );
    let initial_findings = report
        .get("counts")
        .and_then(|counts| counts.get("initialFindings"))
        .and_then(Value::as_u64)
        .unwrap_or(findings.len() as u64);
    assert!(
        initial_findings > 0,
        "code-review counts should record at least one initial finding: {report:#}"
    );
    Ok(())
}

fn snapshot_global_workflows(codex_home: &Path) -> Result<Vec<String>> {
    let root = codex_home.join("workflows");
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut entries = WalkDir::new(&root)
        .min_depth(/*depth*/ 1)
        .max_depth(/*depth*/ 3)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path().display().to_string())
        .collect::<Vec<_>>();
    entries.sort();
    Ok(entries)
}

fn snapshot_fixture_outside_workflow(root: &Path, workflow_dir: &Path) -> Result<Vec<String>> {
    let workflow_dir = workflow_dir.canonicalize()?;
    let mut entries = Vec::new();
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_map(std::result::Result::ok)
    {
        let path = entry.path();
        if path == root {
            continue;
        }
        let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if resolved.starts_with(&workflow_dir) {
            continue;
        }
        let relative = slash_path(path.strip_prefix(root)?);
        let metadata = fs::symlink_metadata(path)?;
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            entries.push(format!(
                "L {relative} -> {}",
                fs::read_link(path)?.display()
            ));
        } else if file_type.is_dir() {
            entries.push(format!("D {relative}/"));
        } else if file_type.is_file() {
            let digest = sha2::Sha256::digest(fs::read(path)?);
            entries.push(format!("F {digest:x} {relative}"));
        }
    }
    entries.sort();
    Ok(entries)
}

fn assert_generated_tests_replaced(workflow_dir: &Path) -> Result<()> {
    for (relative, is_placeholder, message) in [
        (
            "src/tests/workflow.load.test.ts",
            compact_without_comments(workflow_dir.join("src/tests/workflow.load.test.ts"))?
                == "export{};",
            "load test still only exports an empty module",
        ),
        (
            "src/tests/workflow.autocomplete.test.ts",
            {
                let compact = compact_without_comments(
                    workflow_dir.join("src/tests/workflow.autocomplete.test.ts"),
                )?;
                compact.contains("assert.deepEqual(suggestions,[]);")
                    || compact.contains("assert.deepStrictEqual(suggestions,[]);")
            },
            "autocomplete test still asserts an empty suggestion list",
        ),
        (
            "src/tests/workflow.positive.test.ts",
            compact_without_comments(workflow_dir.join("src/tests/workflow.positive.test.ts"))?
                .contains("{ok:true,input"),
            "positive test still asserts scaffold echo output",
        ),
    ] {
        if is_placeholder {
            bail!("{message}: {relative}");
        }
    }
    Ok(())
}

fn compact_without_comments(path: PathBuf) -> Result<String> {
    let compact = fs::read_to_string(path)?
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<String>()
        .split_whitespace()
        .collect::<String>();
    Ok(compact)
}

fn init_git_repo(root: &Path) -> Result<()> {
    assert_process_success(
        Command::new("git")
            .arg("-C")
            .arg(root)
            .arg("init")
            .arg("-b")
            .arg("main")
            .output()?,
        "git init -b main",
    )?;
    assert_process_success(
        Command::new("git")
            .arg("-C")
            .arg(root)
            .arg("add")
            .arg(".")
            .output()?,
        "git add",
    )?;
    assert_process_success(
        Command::new("git")
            .arg("-C")
            .arg(root)
            .arg("-c")
            .arg("user.name=Codex")
            .arg("-c")
            .arg("user.email=codex@openai.com")
            .arg("commit")
            .arg("-m")
            .arg("fixture")
            .output()?,
        "git commit fixture",
    )?;
    Ok(())
}

fn corrupt_workflow_id(contents: &str, corrupted_id: &str) -> Result<String> {
    let mut replaced = false;
    let mut lines = Vec::new();
    for line in contents.lines() {
        if !replaced && line.trim_start().starts_with("id:") {
            let indent_len = line.len() - line.trim_start().len();
            lines.push(format!("{}id: {corrupted_id}", &line[..indent_len]));
            replaced = true;
        } else {
            lines.push(line.to_string());
        }
    }
    if !replaced {
        bail!("failed to corrupt workflow.yaml id");
    }
    Ok(format!("{}\n", lines.join("\n")))
}

fn write_api_key_auth(auth_path: &Path, api_key: &str) -> Result<()> {
    fs::write(
        auth_path,
        serde_json::to_string_pretty(&json!({
            "auth_mode": "apikey",
            "OPENAI_API_KEY": api_key,
            "tokens": null,
            "last_refresh": null,
        }))? + "\n",
    )?;
    Ok(())
}

fn write_chatgpt_token_auth(
    auth_path: &Path,
    tokens: &serde_json::Map<String, Value>,
) -> Result<()> {
    let mut isolated_tokens = Value::Object(tokens.clone());
    isolated_tokens["refresh_token"] = json!("");
    fs::write(
        auth_path,
        serde_json::to_string_pretty(&json!({
            "auth_mode": "chatgptAuthTokens",
            "OPENAI_API_KEY": null,
            "tokens": isolated_tokens,
            "last_refresh": Utc::now().to_rfc3339(),
        }))? + "\n",
    )?;
    Ok(())
}

fn optional_env_secret(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn config_provider_token(codex_home: &Path, provider: &str) -> Option<String> {
    let contents = fs::read_to_string(codex_home.join("config.toml")).ok()?;
    let value: toml::Value = toml::from_str(&contents).ok()?;
    value
        .get("model_providers")?
        .get(provider)?
        .get("token")?
        .as_str()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(ToString::to_string)
}

fn auth_candidates(codex_home: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut candidates = vec![("default".to_string(), codex_home.join("auth.json"))];
    let accounts_dir = codex_home.join("accounts");
    if accounts_dir.is_dir() {
        let mut account_dirs = fs::read_dir(accounts_dir)?
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.path().is_dir())
            .collect::<Vec<_>>();
        account_dirs.sort_by_key(std::fs::DirEntry::file_name);
        candidates.extend(account_dirs.into_iter().map(|entry| {
            (
                format!("account:{}", entry.file_name().to_string_lossy()),
                entry.path().join("auth.json"),
            )
        }));
    }
    Ok(candidates)
}

fn read_json_file(path: &Path) -> Result<Option<Value>> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(Some(serde_json::from_str(&contents)?)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("failed to read {}", path.display())),
    }
}

fn token_expires_at(access_token: &str) -> Result<i64> {
    let payload_segment = access_token
        .split('.')
        .nth(1)
        .context("access token is not a JWT")?;
    let mut padded = payload_segment.to_string();
    while padded.len() % 4 != 0 {
        padded.push('=');
    }
    let decoded = URL_SAFE.decode(padded)?;
    let payload: Value = serde_json::from_slice(&decoded)?;
    payload["exp"]
        .as_i64()
        .context("access token payload does not contain numeric exp")
}

fn auth_secrets(auth_path: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let Some(auth) = read_json_file(auth_path)? else {
        return Ok(Vec::new());
    };
    let mut secrets = Vec::new();
    if let Some(api_key) = auth["OPENAI_API_KEY"]
        .as_str()
        .filter(|api_key| !api_key.trim().is_empty())
    {
        secrets.push(("API key".to_string(), api_key.as_bytes().to_vec()));
    }
    if let Some(access_token) = auth["tokens"]["access_token"]
        .as_str()
        .filter(|token| !token.trim().is_empty())
    {
        secrets.push((
            "ChatGPT access token".to_string(),
            access_token.as_bytes().to_vec(),
        ));
    }
    Ok(secrets)
}

fn real_codex_home() -> Result<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
        .context("CODEX_HOME is unset and HOME is unavailable")
}

fn assert_process_success(output: Output, context: &str) -> Result<()> {
    if !output.status.success() {
        bail!(
            "{context} failed with status {}:\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

fn combined_output(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn path_str(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn slash_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn toml_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

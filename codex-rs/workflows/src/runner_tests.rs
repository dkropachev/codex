use std::fs;
use std::path::Path;
use std::process::ExitStatus;
use std::time::Duration;
use std::time::Instant;

use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;

use super::*;
use crate::CompletionMode;
use crate::ScaffoldRequest;
use crate::ValidationCoverage;
use crate::ValidationPolicy;
use crate::WorkflowPackage;
use crate::scaffold_workflow;

const BUN_REQUIRED: &str = "requires Bun; run explicitly in workflow-runtime validation";

#[test]
#[ignore = "requires Bun; run explicitly in workflow-runtime validation"]
fn scaffold_runs_unchanged_with_shared_runner() {
    let registry = tempfile::tempdir().expect("create workflow registry");
    let root = scaffold_workflow(
        registry.path(),
        &ScaffoldRequest {
            id: "runner-test".to_string(),
            title: "Runner Test".to_string(),
            callable_name: "runner-test".to_string(),
            description: "Exercise the shared runner.".to_string(),
        },
    )
    .expect("scaffold workflow");
    let package = WorkflowPackage::load(&root).expect("load scaffolded package");

    let inspection = inspect_workflow(&root, &package.manifest).expect(BUN_REQUIRED);
    assert_eq!(inspection.api_version, 1);
    assert_eq!(inspection.id, "runner-test");
    assert_eq!(inspection.callable_name, "runner-test");

    let (status, output) = run_output(&root, &package.manifest, &json!({ "message": "Ready." }));
    assert!(status.success());
    assert_eq!(
        String::from_utf8(output).expect("markdown is UTF-8"),
        "# Runner Test\n\nReady.\n"
    );
}

#[cfg(unix)]
#[test]
fn cli_rejects_a_clean_runner_exit_before_completion() {
    use std::os::unix::fs::PermissionsExt;

    let (root, manifest) = write_fixture(&canonical_source(
        "return { message: input.message };",
        "return [];",
    ));
    let bin = tempfile::tempdir().expect("create fake Bun directory");
    let fake_bun = bin.path().join("bun");
    fs::write(&fake_bun, "#!/bin/sh\nexit 0\n").expect("write fake Bun");
    let mut permissions = fs::metadata(&fake_bun)
        .expect("read fake Bun metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_bun, permissions).expect("make fake Bun executable");
    let mut markdown = Vec::new();

    let error = run_cli_workflow_with_bun(
        &fake_bun,
        root.path(),
        &manifest,
        &json!({ "message": "ignored" }),
        &mut markdown,
    )
    .expect_err("clean early exit must not be success");

    assert_eq!(
        error.to_string(),
        "workflow command exited without formatted markdown"
    );
    assert_eq!(markdown, Vec::<u8>::new());
}

#[test]
fn sync_control_reader_bounds_lines_before_allocating_the_whole_frame() {
    let input = format!("{}\nnext\n", "x".repeat(64));
    let mut reader = SyncControlReader::new(std::io::Cursor::new(input.into_bytes()));

    let oversized = reader
        .next_line(/*maximum_bytes*/ 16)
        .expect("read oversized line")
        .expect("line");
    assert!(oversized.oversized);
    assert_eq!(oversized.bytes, vec![b'x'; 16]);
    let next = reader
        .next_line(/*maximum_bytes*/ 16)
        .expect("read next line")
        .expect("line");
    assert!(!next.oversized);
    assert_eq!(next.bytes, b"next");
}

#[cfg(unix)]
#[test]
fn bounded_command_terminates_descendants_after_child_exit() {
    for expected_code in [0, 17] {
        let directory = tempfile::tempdir().expect("create process fixture");
        let pid_path = directory.path().join("descendant.pid");
        let command = descendant_command(
            &pid_path,
            &format!("sleep 60 & echo $! > \"$1\"; exit {expected_code}"),
        );

        let (status, _, _, _) = run_bounded_command(
            command,
            Duration::from_secs(/*secs*/ 2),
            /*maximum_stdout_bytes*/ 1024,
            /*cancelled*/ None,
        )
        .expect("run bounded command");
        let descendant = read_descendant_pid(&pid_path);

        assert_eq!(status.code(), Some(expected_code));
        assert_process_terminated(descendant);
    }
}

#[cfg(unix)]
#[test]
fn bounded_command_terminates_descendants_on_timeout() {
    let directory = tempfile::tempdir().expect("create process fixture");
    let pid_path = directory.path().join("descendant.pid");
    let command = descendant_command(&pid_path, "sleep 60 & echo $! > \"$1\"; wait");

    let error = run_bounded_command(
        command,
        Duration::from_secs(/*secs*/ 1),
        /*maximum_stdout_bytes*/ 1024,
        /*cancelled*/ None,
    )
    .expect_err("command must time out");
    let descendant = read_descendant_pid(&pid_path);

    assert!(error.to_string().contains("timed out"));
    assert_process_terminated(descendant);
}

#[cfg(unix)]
#[test]
fn bounded_command_terminates_descendants_on_cancellation() {
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering;

    let directory = tempfile::tempdir().expect("create process fixture");
    let pid_path = directory.path().join("descendant.pid");
    let command = descendant_command(&pid_path, "sleep 60 & echo $! > \"$1\"; wait");
    let cancelled = AtomicBool::new(false);

    let error = std::thread::scope(|scope| {
        scope.spawn(|| {
            let _ = read_descendant_pid(&pid_path);
            cancelled.store(true, Ordering::Relaxed);
        });
        run_bounded_command(
            command,
            Duration::from_secs(/*secs*/ 5),
            /*maximum_stdout_bytes*/ 1024,
            Some(&cancelled),
        )
        .expect_err("command must be cancelled")
    });
    let descendant = read_descendant_pid(&pid_path);

    assert_eq!(error.to_string(), "workflow runner was cancelled");
    assert_process_terminated(descendant);
}

#[cfg(unix)]
#[test]
fn workflow_child_guard_terminates_descendants_on_drop() {
    let directory = tempfile::tempdir().expect("create process fixture");
    let pid_path = directory.path().join("descendant.pid");
    let mut command = descendant_command(&pid_path, "sleep 60 & echo $! > \"$1\"; wait");
    let child = WorkflowChildGuard::spawn(&mut command).expect("spawn guarded command");
    let descendant = read_descendant_pid(&pid_path);

    drop(child);

    assert_process_terminated(descendant);
}

#[test]
#[ignore = "requires Bun; run explicitly in workflow-runtime validation"]
fn cli_run_imports_the_module_once_and_omits_optional_interaction() {
    let (root, manifest) = write_fixture(&format!(
        "import fs from \"node:fs\";\nconst statePath = \"load-count\";\nconst loadCount = Number(fs.existsSync(statePath) ? fs.readFileSync(statePath, \"utf8\") : \"0\") + 1;\nfs.writeFileSync(statePath, String(loadCount));\n{}",
        canonical_source(
            r#"if ("requestUserInput" in (_ctx as object)) throw new Error("CLI context exposed requestUserInput");
    return { message: String(loadCount) };"#,
            "return [];",
        )
    ));

    let (status, markdown) = run_output(root.path(), &manifest, &json!({ "message": "ignored" }));
    assert!(status.success());
    assert_eq!(markdown, b"1\n");
    assert_eq!(
        fs::read_to_string(root.path().join("load-count")).expect("read load count"),
        "1"
    );
}

#[test]
#[ignore = "requires Bun; run explicitly in workflow-runtime validation"]
fn cli_terminates_a_runner_that_stays_alive_after_completion() {
    let (root, manifest) = write_fixture(&format!(
        "setInterval(() => {{}}, 1000);\n{}",
        canonical_source("return { message: input.message };", "return [];",)
    ));
    let mut markdown = Vec::new();
    let started = Instant::now();

    let error = run_cli_workflow_with_bun(
        Path::new("bun"),
        root.path(),
        &manifest,
        &json!({ "message": "Ready." }),
        &mut markdown,
    )
    .expect_err("runner with an open handle must be terminated");

    assert!(started.elapsed() < Duration::from_secs(4));
    assert!(
        error
            .to_string()
            .contains("did not exit within 2000 ms after completion")
    );
    assert_eq!(markdown, Vec::<u8>::new());
}

#[test]
#[ignore = "requires Bun; run explicitly in workflow-runtime validation"]
fn cli_runner_rejects_input_and_output_schema_violations() {
    let (input_root, manifest) = write_fixture(&canonical_source(
        "return { message: input.message };",
        "return [];",
    ));
    let (status, markdown) = run_output(input_root.path(), &manifest, &json!({ "message": 7 }));
    assert!(!status.success(), "invalid input must fail the Bun runner");
    assert_eq!(markdown, Vec::<u8>::new());

    let (output_root, manifest) =
        write_fixture(&canonical_source("return { message: 7 };", "return [];"));
    let (status, markdown) = run_output(
        output_root.path(),
        &manifest,
        &json!({ "message": "Ready." }),
    );
    assert!(!status.success(), "invalid output must fail the Bun runner");
    assert_eq!(markdown, Vec::<u8>::new());
}

#[test]
#[ignore = "requires Bun; run explicitly in workflow-runtime validation"]
fn runner_handles_large_input_without_command_line_payloads() {
    let registry = tempfile::tempdir().expect("create workflow registry");
    let root = scaffold_workflow(
        registry.path(),
        &ScaffoldRequest {
            id: "large-input".to_string(),
            title: "Large Input".to_string(),
            callable_name: "large-input".to_string(),
            description: "Exercise file-backed input transport.".to_string(),
        },
    )
    .expect("scaffold workflow");
    let source_path = root.join("src/workflow.ts");
    let source = fs::read_to_string(&source_path).expect("read workflow source");
    fs::write(
        &source_path,
        source
            .replace("message?: string;", "message?: string;\n  payload?: string;")
            .replace(
                "message: { type: \"string\", description: \"Message to include in the result.\" },",
                "message: { type: \"string\", description: \"Message to include in the result.\" },\n    payload: { type: \"string\" },",
            ),
    )
    .expect("extend workflow input schema");
    let package = WorkflowPackage::load(&root).expect("load package");
    let (status, _) = run_output(
        &root,
        &package.manifest,
        &json!({ "message": "Ready.", "payload": "x".repeat(256 * 1024) }),
    );
    assert!(status.success());
}

#[test]
#[ignore = "requires Bun; run explicitly in workflow-runtime validation"]
fn inspect_rejects_malformed_default_export() {
    let (root, manifest) = write_fixture(
        r#"export const inputSchema = {};
export const outputSchema = {};
export default async function workflow() { return {}; }
"#,
    );
    let error =
        inspect_workflow(root.path(), &manifest).expect_err("function export must be rejected");
    assert!(
        format!("{error:#}").contains("Workflow must have a default object export"),
        "unexpected error: {error:#}"
    );
}

#[test]
#[ignore = "requires Bun; run explicitly in workflow-runtime validation"]
fn inspect_rejects_non_json_schema_values_before_transport() {
    let source = canonical_source("return { message: input.message };", "return [];").replace(
        "message: { type: \"string\" },",
        "message: { type: undefined },",
    );
    let (root, manifest) = write_fixture(&source);

    let error = inspect_workflow(root.path(), &manifest)
        .expect_err("undefined schema values must be rejected");
    assert!(
        format!("{error:#}").contains("must be JSON-serializable"),
        "unexpected error: {error:#}"
    );

    let source = canonical_source("return { message: input.message };", "return [];").replace(
        "properties: { message: { type: \"string\" } },",
        "properties: new Map([[\"message\", { type: \"number\" }]]),",
    );
    let (root, manifest) = write_fixture(&source);
    let error =
        inspect_workflow(root.path(), &manifest).expect_err("Map schema values must be rejected");
    assert!(
        format!("{error:#}").contains("must be a plain JSON object"),
        "unexpected error: {error:#}"
    );

    let source = canonical_source("return { message: input.message };", "return [];")
        .replace(
            "export const inputSchema = {",
            "const defaultInputSchema = {",
        )
        .replacen(
            "  additionalProperties: false,\n} as const;",
            "  additionalProperties: false,\n} as const;\nexport const inputSchema = new Map();",
            1,
        )
        .replace(
            "  inputSchema,\n  outputSchema,",
            "  inputSchema: defaultInputSchema,\n  outputSchema,",
        );
    let (root, manifest) = write_fixture(&source);
    let error = inspect_workflow(root.path(), &manifest)
        .expect_err("non-JSON named schema exports must be rejected");
    assert!(
        format!("{error:#}").contains("Named inputSchema export must be JSON-serializable"),
        "unexpected error: {error:#}"
    );
}

#[test]
#[ignore = "requires Bun; run explicitly in workflow-runtime validation"]
fn inspect_rejects_metadata_mismatch() {
    let (root, mut manifest) = write_fixture(&canonical_source(
        "return { message: input.message };",
        "return [];",
    ));
    manifest.id = "different-id".to_string();

    let error = inspect_workflow(root.path(), &manifest).expect_err("reject metadata mismatch");
    assert!(
        format!("{error:#}").contains("does not match workflow.yaml id"),
        "unexpected error: {error:#}"
    );
}

#[test]
#[ignore = "requires Bun; run explicitly in workflow-runtime validation"]
fn completion_hook_is_timed_out_and_bounded() {
    let registry = tempfile::tempdir().expect("create workflow registry");
    let timeout_root = scaffold_workflow(
        registry.path(),
        &ScaffoldRequest {
            id: "runner-timeout".to_string(),
            title: "Runner Timeout".to_string(),
            callable_name: "runner-timeout".to_string(),
            description: "Exercise completion timeout fallback.".to_string(),
        },
    )
    .expect("scaffold timeout workflow");
    let source_path = timeout_root.join("src/workflow.ts");
    let source = fs::read_to_string(&source_path).expect("read scaffold source");
    let source = source.replace("return [];", "return await new Promise(() => {});");
    fs::write(&source_path, source).expect("write hanging completion hook");
    let request = CompletionRequest {
        input: json!({}),
        active_field: None,
        prefix: "--m".to_string(),
        mode: CompletionMode::Field,
    };
    let started = Instant::now();
    let completion = crate::complete_workflow(&timeout_root, &request);
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(completion.items.len(), 1);
    assert_eq!(completion.items[0].value, "--message");
    let error = completion
        .error
        .expect("dynamic timeout should be reported");
    assert!(
        error.contains("timed out"),
        "unexpected completion error: {error}"
    );

    let request = CompletionRequest {
        input: json!({}),
        active_field: None,
        prefix: String::new(),
        mode: CompletionMode::Field,
    };
    let (bounded_root, manifest) = write_fixture(&canonical_source(
        "return { message: input.message };",
        r#"return Array.from({ length: 150 }, (_, index) => ({
      value: `value-${index.toString().padStart(3, "0")}`,
    }));"#,
    ));
    let completions =
        run_completion_hook(bounded_root.path(), &manifest, &request).expect(BUN_REQUIRED);
    assert_eq!(completions.len(), 100);
    assert_eq!(completions[0].value, "value-000");
    assert_eq!(completions[99].value, "value-099");
}

#[test]
#[ignore = "requires Bun; run explicitly in workflow-runtime validation"]
fn completion_imports_the_module_once() {
    let (root, manifest) = write_fixture(&format!(
        "import fs from \"node:fs\";\nconst statePath = \"load-count\";\nconst loadCount = Number(fs.existsSync(statePath) ? fs.readFileSync(statePath, \"utf8\") : \"0\") + 1;\nfs.writeFileSync(statePath, String(loadCount));\n{}",
        canonical_source(
            "return { message: input.message };",
            r#"return [{ value: `load-${loadCount}` }];"#,
        )
    ));
    let request = CompletionRequest {
        input: json!({}),
        active_field: None,
        prefix: String::new(),
        mode: CompletionMode::Field,
    };

    let output = run_completion_operation_cancellable(
        root.path(),
        &manifest,
        &request,
        &std::sync::atomic::AtomicBool::new(false),
    )
    .expect(BUN_REQUIRED);

    assert_eq!(
        output.items,
        vec![CompletionItem {
            value: "load-1".to_string(),
            description: None,
        }]
    );
    assert_eq!(
        fs::read_to_string(root.path().join("load-count")).expect("read load count"),
        "1"
    );
}

#[test]
#[ignore = "requires Bun; run explicitly in workflow-runtime validation"]
fn completion_does_not_import_a_package_that_fails_executable_validation() {
    let registry = tempfile::tempdir().expect("create workflow registry");
    let root = scaffold_workflow(
        registry.path(),
        &ScaffoldRequest {
            id: "unsafe-completion".to_string(),
            title: "Unsafe Completion".to_string(),
            callable_name: "unsafe-completion".to_string(),
            description: "Ensure unsafe completion modules are not imported.".to_string(),
        },
    )
    .expect("scaffold workflow");
    let source_path = root.join("src/workflow.ts");
    let source = fs::read_to_string(&source_path).expect("read scaffold source");
    fs::write(
        &source_path,
        format!(
            "import {{ writeFileSync }} from 'node:fs';\nwriteFileSync('state/imported', 'yes');\nconst unsafeSpecifier = './unsafe';\nvoid import(unsafeSpecifier);\n{source}"
        ),
    )
    .expect("write workflow import side effect");

    let completion = crate::complete_workflow(
        &root,
        &CompletionRequest {
            input: json!({}),
            active_field: None,
            prefix: String::new(),
            mode: CompletionMode::Field,
        },
    );

    assert_eq!(completion.items, Vec::<CompletionItem>::new());
    assert!(
        completion
            .error
            .is_some_and(|error| error.contains("non-literal dynamic import or require"))
    );
    assert!(!root.join("state/imported").exists());
}

#[test]
#[ignore = "requires Bun; run explicitly in workflow-runtime validation"]
fn source_scan_bounds_entries_depth_and_file_size() {
    let (root, _) = write_fixture(&canonical_source(
        "return { message: input.message };",
        "return [];",
    ));
    for index in 0..1_024 {
        fs::write(root.path().join("src").join(format!("ignored-{index}")), "")
            .expect("write source entry");
    }
    let error = scan_workflow_sources(root.path()).expect_err("entry limit must be enforced");
    assert!(format!("{error:#}").contains("exceeds 1024 directory entries"));

    let (root, _) = write_fixture(&canonical_source(
        "return { message: input.message };",
        "return [];",
    ));
    let mut directory = root.path().join("src");
    for _ in 0..=32 {
        directory.push("nested");
        fs::create_dir(&directory).expect("create nested source directory");
    }
    let error = scan_workflow_sources(root.path()).expect_err("depth limit must be enforced");
    assert!(format!("{error:#}").contains("exceeds 32 directory levels"));

    let (root, _) = write_fixture(&canonical_source(
        "return { message: input.message };",
        "return [];",
    ));
    fs::write(
        root.path().join("src/oversized.ts"),
        vec![b' '; 1024 * 1024 + 1],
    )
    .expect("write oversized source");
    let error = scan_workflow_sources(root.path()).expect_err("byte limit must be enforced");
    assert!(format!("{error:#}").contains("exceeds 1048576 bytes"));
}

fn run_output(root: &Path, manifest: &WorkflowManifest, input: &Value) -> (ExitStatus, Vec<u8>) {
    let mut markdown = Vec::new();
    let status = run_cli_workflow_with_bun(Path::new("bun"), root, manifest, input, &mut markdown)
        .expect(BUN_REQUIRED);
    (status, markdown)
}

fn write_fixture(source: &str) -> (tempfile::TempDir, WorkflowManifest) {
    let root = tempfile::tempdir().expect("create workflow fixture");
    fs::create_dir(root.path().join("src")).expect("create source directory");
    fs::write(root.path().join("src/workflow.ts"), source).expect("write workflow module");
    (root, fixture_manifest())
}

fn fixture_manifest() -> WorkflowManifest {
    WorkflowManifest {
        api_version: 1,
        id: "runner-test".to_string(),
        title: "Runner Test".to_string(),
        callable_name: "runner-test".to_string(),
        description: "Runner fixture".to_string(),
        validation: ValidationPolicy {
            commands: Vec::new(),
            coverage: ValidationCoverage {
                positive: true,
                load: true,
                autocomplete: true,
                negative: true,
                recovery: false,
            },
        },
    }
}

fn canonical_source(run_body: &str, complete_body: &str) -> String {
    format!(
        r#"export interface WorkflowInput {{ message?: string; }}
export interface WorkflowOutput {{ message: string; }}

export const inputSchema = {{
  $schema: "https://json-schema.org/draft/2020-12/schema",
  type: "object",
  properties: {{
    workingDirectory: {{ type: "string" }},
    message: {{ type: "string" }},
  }},
  additionalProperties: false,
}} as const;

export const outputSchema = {{
  $schema: "https://json-schema.org/draft/2020-12/schema",
  type: "object",
  properties: {{ message: {{ type: "string" }} }},
  required: ["message"],
  additionalProperties: false,
}} as const;

export default {{
  apiVersion: 1,
  id: "runner-test",
  title: "Runner Test",
  callableName: "runner-test",
  inputSchema,
  outputSchema,
  async run(_ctx: unknown, input: WorkflowInput): Promise<WorkflowOutput> {{
    {run_body}
  }},
  async complete(_ctx: unknown, _request: unknown) {{
    {complete_body}
  }},
  async format(output: WorkflowOutput, options: {{ format: "markdown.v1" }}) {{
    if (options.format !== "markdown.v1") throw new Error("unsupported format");
    return {{ markdown: `${{output.message}}\n` }};
  }},
}};
"#
    )
}

#[cfg(unix)]
fn descendant_command(pid_path: &Path, script: &str) -> std::process::Command {
    let mut command = std::process::Command::new("sh");
    command
        .args(["-c", script, "workflow-process-test"])
        .arg(pid_path);
    command
}

#[cfg(unix)]
fn read_descendant_pid(pid_path: &Path) -> rustix::process::Pid {
    let deadline = Instant::now() + Duration::from_secs(/*secs*/ 2);
    loop {
        if let Ok(encoded) = fs::read_to_string(pid_path)
            && let Ok(raw_pid) = encoded.trim().parse::<i32>()
            && let Some(pid) = rustix::process::Pid::from_raw(raw_pid)
        {
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "descendant did not publish its process ID"
        );
        std::thread::sleep(Duration::from_millis(/*millis*/ 10));
    }
}

#[cfg(unix)]
fn assert_process_terminated(pid: rustix::process::Pid) {
    let deadline = Instant::now() + Duration::from_secs(/*secs*/ 2);
    while rustix::process::test_kill_process(pid).is_ok() {
        assert!(
            Instant::now() < deadline,
            "descendant process {pid} remained alive"
        );
        std::thread::sleep(Duration::from_millis(/*millis*/ 10));
    }
}

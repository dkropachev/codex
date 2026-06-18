use super::*;
use pretty_assertions::assert_eq;

fn repeated_passing_tests(count: usize) -> String {
    let mut lines = vec!["running 500 tests".to_string()];
    for idx in 0..count {
        lines.push(format!("test crate::module::passes_{idx} ... ok"));
    }
    lines.push("test result: ok. 500 passed; 0 failed; 0 ignored; finished in 1.23s".to_string());
    lines.join("\n")
}

#[test]
fn cargo_success_output_collapses_passing_noise() {
    let output = repeated_passing_tests(/*count*/ 500);
    let compacted = compact_output(&["cargo test".to_string()], output.as_str())
        .expect("cargo test output should compact");

    assert_eq!(compacted.filter_id, "cargo-test-v1");
    assert!(compacted.text.contains("test result: ok"));
    assert!(!compacted.text.contains("passes_499"));
    assert!(compacted.compacted_token_count < compacted.original_token_count);
}

#[test]
fn cargo_test_failure_keeps_failing_details() {
    let mut output = repeated_passing_tests(/*count*/ 450);
    output.push_str(
        r#"
failures:

---- tests::keeps_failure stdout ----
thread 'tests::keeps_failure' panicked at src/lib.rs:42:9:
expected left == right
  left: 1
 right: 2

failures:
    tests::keeps_failure

test result: FAILED. 450 passed; 1 failed; finished in 1.23s
error: test failed, to rerun pass `-p codex-core --lib`
"#,
    );

    let compacted = compact_output(&["cargo test".to_string()], output.as_str())
        .expect("cargo test failure output should compact");

    assert_eq!(compacted.filter_id, "cargo-test-v1");
    assert!(compacted.text.contains("tests::keeps_failure"));
    assert!(compacted.text.contains("panicked at src/lib.rs:42:9"));
    assert!(compacted.text.contains("test result: FAILED"));
}

#[test]
fn compiler_diagnostics_retain_file_line_and_error_blocks() {
    let mut output = String::new();
    for idx in 0..500 {
        output.push_str(&format!("   Compiling dependency_{idx} v0.1.0\n"));
    }
    output.push_str(
        r#"error[E0425]: cannot find value `missing` in this scope
  --> src/main.rs:12:5
   |
12 |     missing;
   |     ^^^^^^^ not found in this scope

warning: `example` generated 1 warning
error: could not compile `example` due to previous error
"#,
    );

    let compacted = compact_output(&["cargo build".to_string()], output.as_str())
        .expect("cargo build output should compact");

    assert_eq!(compacted.filter_id, "cargo-build-v1");
    assert!(compacted.text.contains("error[E0425]"));
    assert!(compacted.text.contains("--> src/main.rs:12:5"));
    assert!(!compacted.text.contains("dependency_100"));
}

#[test]
fn go_success_output_collapses_package_noise() {
    let mut output = String::from(
        r#"=== RUN   TestDefaultPortForConnUsesRemoteTCPPort
=== PAUSE TestDefaultPortForConnUsesRemoteTCPPort
=== RUN   TestDefaultPortForConnFallsBack
=== PAUSE TestDefaultPortForConnFallsBack
=== CONT  TestDefaultPortForConnUsesRemoteTCPPort
--- PASS: TestDefaultPortForConnUsesRemoteTCPPort (0.00s)
=== CONT  TestDefaultPortForConnFallsBack
--- PASS: TestDefaultPortForConnFallsBack (0.00s)
PASS
ok  	github.com/gocql/gocql	0.002s
"#,
    );
    for idx in 0..90 {
        output.push_str("testing: warning: no tests to run\n");
        output.push_str(&format!(
            "ok  \tgithub.com/gocql/gocql/package{idx}\t0.001s [no tests to run]\n"
        ));
        output.push_str(&format!(
            "?   \tgithub.com/gocql/gocql/empty{idx}\t[no test files]\n"
        ));
    }

    let compacted = compact_output(&["go test -v ./...".to_string()], output.as_str())
        .expect("go test output should compact");

    assert_eq!(compacted.filter_id, "go-test-v1");
    assert!(compacted.text.contains("go test output: PASS"));
    assert!(compacted.text.contains("2 tests passed"));
    assert!(compacted.text.contains("90 packages with no test files"));
    assert!(!compacted.text.contains("package89"));
}

#[test]
fn maven_success_output_collapses_lifecycle_noise() {
    let mut output = String::from(
        "\u{1b}[1;34m[INFO]\u{1b}[m Scanning for projects...\n\
         \u{1b}[1;34m[INFO]\u{1b}[m -------------------------------------------------------\n\
         \u{1b}[1;34m[INFO]\u{1b}[m  T E S T S\n\
         \u{1b}[1;34m[INFO]\u{1b}[m -------------------------------------------------------\n\
         \u{1b}[1;34m[INFO]\u{1b}[m Running com.example.UpdateFluentAssignmentTest\n",
    );
    for idx in 0..160 {
        output.push_str(&format!(
            "Downloading from central: https://repo.maven.apache.org/maven2/example/artifact{idx}.pom\n"
        ));
        output.push_str(&format!("Progress (1): {idx}/160 kB\r"));
        output.push_str(&format!(
            "[INFO] --- compiler:3.15.0:compile (default-compile) @ module{idx} ---\n"
        ));
    }
    output.push_str(
        "\
[WARNING] Test source directory '/tmp/project/src/test/java' does not exist, ignoring.\n\
[INFO] Tests run: 8, Failures: 0, Errors: 0, Skipped: 0, Time elapsed: 0.191 s -- in com.example.UpdateFluentAssignmentTest\n\
[INFO] Results:\n\
[INFO] Tests run: 8, Failures: 0, Errors: 0, Skipped: 0\n\
[INFO] Reactor Summary for Example:\n\
[INFO] module-a ....................................... SUCCESS [  0.309 s]\n\
[INFO] module-b ....................................... SUCCESS [  0.675 s]\n\
[INFO] BUILD SUCCESS\n\
[INFO] Total time:  17.450 s\n",
    );

    let compacted = compact_output(&["mvn -pl query-builder test".to_string()], output.as_str())
        .expect("maven output should compact");

    assert_eq!(compacted.filter_id, "maven-v1");
    assert!(compacted.text.contains("Maven output: BUILD SUCCESS."));
    assert!(
        compacted
            .text
            .contains("Tests run: 8, Failures: 0, Errors: 0, Skipped: 0")
    );
    assert!(compacted.text.contains("Warnings omitted: 1"));
    assert!(!compacted.text.contains("artifact159"));
}

#[test]
fn maven_failure_output_keeps_failing_test_details() {
    let mut output = String::new();
    for idx in 0..180 {
        output.push_str(&format!(
            "[INFO] --- plugin:goal @ passing-module-{idx} ---\n"
        ));
    }
    output.push_str(
        r#"[INFO] Running com.example.CounterAssignmentTest
[ERROR] Tests run: 1, Failures: 1, Errors: 0, Skipped: 0, Time elapsed: 0.01 s <<< FAILURE! -- in com.example.CounterAssignmentTest
[ERROR] com.example.CounterAssignmentTest.negativeCounterDelta -- Time elapsed: 0.01 s <<< FAILURE!
org.opentest4j.AssertionFailedError:
expected: <c=c-2>
 but was: <c=c+-2>
        at com.example.CounterAssignmentTest.negativeCounterDelta(CounterAssignmentTest.java:42)
[ERROR] Failures:
[ERROR]   CounterAssignmentTest.negativeCounterDelta:42 expected: <c=c-2> but was: <c=c+-2>
[ERROR] There are test failures.
[INFO] BUILD FAILURE
"#,
    );

    let compacted = compact_output(&["mvn test".to_string()], output.as_str())
        .expect("maven failure output should compact");

    assert_eq!(compacted.filter_id, "maven-v1");
    assert!(compacted.text.contains("CounterAssignmentTest"));
    assert!(compacted.text.contains("expected: <c=c-2>"));
    assert!(compacted.text.contains("BUILD FAILURE"));
    assert!(!compacted.text.contains("passing-module-100"));
}

#[test]
fn nextest_success_output_from_session_history_collapses_pass_lines() {
    let mut output = String::from(
        "RUST_MIN_STACK=8388608 cargo nextest run --no-fail-fast \"$@\"\n\
         Compiling codex-core v0.139.0 (/extra/dkropachev/codex-3/codex-rs/core)\n\
         Checking codex-tui v0.139.0 (/extra/dkropachev/codex-3/codex-rs/tui)\n\
         Finished `test` profile [unoptimized + debuginfo] target(s) in 1m 16s\n\
         ------------\n\
         Nextest run ID b5208a11-c184-4e2e-af9f-07885452e6fa with nextest profile: default\n\
         Starting 10773 tests across 192 binaries (22 tests skipped)\n",
    );
    for idx in 0..1800 {
        output.push_str(&format!(
            "        PASS [   0.004s] ({idx:5}/10773) codex-core suite::passes_{idx}\n"
        ));
    }
    output.push_str(
        "        SLOW [> 30.000s] (----------) codex-exec::all suite::resume::exec_resume_last_respects_cwd_filter_and_all_flag\n\
         ------------\n\
              Summary [ 207.643s] 10773 tests run: 10773 passed (1 slow), 22 skipped\n",
    );

    let compacted = compact_output(&["just test".to_string()], output.as_str())
        .expect("nextest success output should compact");

    assert_eq!(compacted.filter_id, "nextest-v1");
    assert!(compacted.text.contains("10773 tests run: 10773 passed"));
    assert!(compacted.text.contains("Slow tests:"));
    assert!(compacted.text.contains("Omitted 1800 passing test lines"));
    assert!(!compacted.text.contains("passes_1799"));
    assert!(compacted.compacted_token_count * 10 < compacted.original_token_count);
}

#[test]
fn nextest_failure_output_stays_raw_until_failure_parser_is_confirmed() {
    let mut output = String::from(
        "Nextest run ID b5208a11-c184-4e2e-af9f-07885452e6fa with nextest profile: default\n",
    );
    for idx in 0..500 {
        output.push_str(&format!(
            "        PASS [   0.004s] ({idx:5}/501) codex-core suite::passes_{idx}\n"
        ));
    }
    output.push_str(
        "        FAIL [   0.050s] codex-core suite::keeps_failure\n\
         ------------\n\
              Summary [ 12.345s] 501 tests run: 500 passed, 1 failed\n",
    );

    assert_eq!(
        compact_output(&["just test".to_string()], output.as_str()),
        None
    );
}

#[test]
fn pytest_success_output_collapses_progress_noise() {
    let mut output = String::new();
    for idx in 0..600 {
        output.push_str(&format!(
            "tests/test_api.py::test_passing_case_{idx} PASSED [{idx:3}%]\n"
        ));
    }
    output.push_str(
        "============================== 600 passed in 12.34s ==============================\n",
    );

    let compacted = compact_output(&["python -m pytest -vv".to_string()], output.as_str())
        .expect("pytest success output should compact");

    assert_eq!(compacted.filter_id, "pytest-v1");
    assert!(compacted.text.contains("600 passed in 12.34s"));
    assert!(!compacted.text.contains("test_passing_case_599"));
    assert!(compacted.compacted_token_count * 10 < compacted.original_token_count);
}

#[test]
fn pytest_failure_output_keeps_failure_and_short_summary() {
    let mut output = String::new();
    for idx in 0..320 {
        output.push_str(&format!(
            "tests/test_query.py::test_passing_case_{idx} PASSED [{idx:3}%]\n"
        ));
    }
    output.push_str(
        r#"=================================== FAILURES ===================================
_______________________________ test_bad_value ________________________________

    def test_bad_value():
>       assert format_counter(-2) == "c=c-2"
E       AssertionError: assert 'c=c+-2' == 'c=c-2'

tests/test_query.py:42: AssertionError
=========================== short test summary info ============================
FAILED tests/test_query.py::test_bad_value - AssertionError: assert 'c=c+-2' == 'c=c-2'
========================= 1 failed, 320 passed in 3.21s =========================
"#,
    );

    let compacted = compact_output(&["pytest -vv".to_string()], output.as_str())
        .expect("pytest failure output should compact");

    assert_eq!(compacted.filter_id, "pytest-v1");
    assert!(compacted.text.contains("test_bad_value"));
    assert!(compacted.text.contains("format_counter(-2)"));
    assert!(compacted.text.contains("1 failed, 320 passed"));
    assert!(!compacted.text.contains("test_passing_case_200"));
}

#[test]
fn typescript_output_groups_diagnostics_by_file_and_code() {
    let mut output = String::new();
    for idx in 0..220 {
        output.push_str(&format!(
            "src/file_{}.ts({},{}): error TS2304: Cannot find name 'missing_{idx}'.\n",
            idx % 5,
            idx + 1,
            idx % 80 + 1
        ));
        output.push_str(&format!("  const value = missing_{idx};\n"));
    }

    let compacted = compact_output(&["tsc --noEmit".to_string()], output.as_str())
        .expect("tsc output should compact");

    assert_eq!(compacted.filter_id, "tsc-v1");
    assert!(
        compacted
            .text
            .contains("TypeScript: 220 diagnostics in 5 files.")
    );
    assert!(compacted.text.contains("TS2304 (220x)"));
    assert!(compacted.text.contains("src/file_0.ts"));
    assert!(!compacted.text.contains("missing_219"));
}

#[test]
fn gradle_output_strips_task_and_download_noise() {
    let mut output =
        String::from("Starting a Gradle Daemon, 1 incompatible Daemon could not be reused\n");
    for idx in 0..220 {
        output.push_str(&format!(
            "Downloading https://repo.example.test/artifact-{idx}.pom\n"
        ));
        output.push_str(&format!("> Task :module{idx}:compileJava UP-TO-DATE\n"));
        output.push_str(&format!("> Transform dependency-{idx}.jar\n"));
    }
    output.push_str(
        "> Task :driver:compileJava FAILED\n\
         /workspace/src/main/java/App.java:12: error: cannot find symbol\n\
         BUILD FAILED in 2s\n",
    );

    let compacted = compact_output(&["./gradlew test".to_string()], output.as_str())
        .expect("gradle output should compact");

    assert_eq!(compacted.filter_id, "gradle-v1");
    assert!(compacted.text.contains(":driver:compileJava FAILED"));
    assert!(compacted.text.contains("cannot find symbol"));
    assert!(compacted.text.contains("BUILD FAILED"));
    assert!(!compacted.text.contains("dependency-219"));
}

#[test]
fn terraform_plan_output_strips_refresh_noise() {
    let mut output = String::from("Acquiring state lock. This may take a few moments...\n");
    for idx in 0..240 {
        output.push_str(&format!(
            "module.node.aws_instance.server[{idx}]: Refreshing state... [id=i-{idx:08}]\n"
        ));
    }
    output.push_str(
        r#"Terraform will perform the following actions:

  # aws_instance.web will be updated in-place
  ~ resource "aws_instance" "web" {
      ~ tags = {
          ~ "version" = "1" -> "2"
        }
    }

Plan: 0 to add, 1 to change, 0 to destroy.
Releasing state lock. This may take a few moments...
"#,
    );

    let compacted = compact_output(&["terraform plan".to_string()], output.as_str())
        .expect("terraform plan output should compact");

    assert_eq!(compacted.filter_id, "terraform-plan-v1");
    assert!(
        compacted
            .text
            .contains("Terraform will perform the following actions")
    );
    assert!(
        compacted
            .text
            .contains("Plan: 0 to add, 1 to change, 0 to destroy.")
    );
    assert!(!compacted.text.contains("Refreshing state"));
}

#[test]
fn tofu_plan_output_strips_refresh_noise() {
    let mut output = String::new();
    for idx in 0..240 {
        output.push_str(&format!(
            "module.node.tofu_resource.server[{idx}]: Refreshing state... [id=node-{idx}]\n"
        ));
    }
    output.push_str(
        r#"OpenTofu will perform the following actions:

  # tofu_resource.web will be created
  + resource "tofu_resource" "web" {
      + name = "web"
    }

Plan: 1 to add, 0 to change, 0 to destroy.
"#,
    );

    let compacted = compact_output(&["tofu plan".to_string()], output.as_str())
        .expect("tofu plan output should compact");

    assert_eq!(compacted.filter_id, "tofu-plan-v1");
    assert!(
        compacted
            .text
            .contains("OpenTofu will perform the following actions")
    );
    assert!(
        compacted
            .text
            .contains("Plan: 1 to add, 0 to change, 0 to destroy.")
    );
    assert!(!compacted.text.contains("Refreshing state"));
}

#[test]
fn uv_sync_up_to_date_output_collapses_cache_noise() {
    let mut output = String::new();
    for idx in 0..260 {
        output.push_str(&format!("Using cached package-{idx}==1.0.{idx}\n"));
    }
    output.push_str("Resolved 171 packages in 1ms\nAudited 171 packages in 2ms\n");

    let compacted = compact_output(&["uv sync --locked".to_string()], output.as_str())
        .expect("uv output should compact");

    assert_eq!(compacted.filter_id, "uv-sync-v1");
    assert_eq!(compacted.text, "uv: ok (up to date)");
    assert!(compacted.compacted_token_count * 10 < compacted.original_token_count);
}

#[test]
fn large_json_summarizes_structure() {
    let records = (0..300)
        .map(|idx| {
            serde_json::json!({
                "id": idx,
                "name": format!("item-{idx}"),
                "nested": { "enabled": idx % 2 == 0, "count": idx },
            })
            .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");

    let compacted = compact_output(&["cat data.jsonl".to_string()], records.as_str())
        .expect("json output should compact");

    assert_eq!(compacted.filter_id, "json-structure-v1");
    assert!(compacted.text.contains("NDJSON: 300 records"));
    assert!(compacted.text.contains("Object keys observed"));
}

#[test]
fn accepted_rg_summary_keeps_files_and_representative_matches() {
    let mut output = String::new();
    for idx in 0..240 {
        output.push_str(&format!(
            "src/module_{}.rs:{}:fn target_{idx}() {{}}\n",
            idx % 12,
            idx + 1
        ));
    }

    let suggestion_keys = vec!["exec.rg-summary-v1".to_string()];
    let compacted = compact_output_for_suggestions(
        &[
            "bash".to_string(),
            "-lc".to_string(),
            "rg -n target src".to_string(),
        ],
        output.as_str(),
        &suggestion_keys,
    )
    .expect("accepted rg summary should compact");

    assert_eq!(compacted.filter_id, "exec.rg-summary-v1");
    assert!(
        compacted
            .text
            .contains("rg summary: 240 matches in 12 files")
    );
    assert!(compacted.text.contains("src/module_0.rs"));
    assert!(compacted.text.contains("Representative matches:"));
    assert!(!compacted.text.contains("target_239"));
    assert!(compacted.compacted_token_count < compacted.original_token_count);
}

#[test]
fn accepted_source_read_dedupe_omits_repeated_source_lines() {
    let output = (10..260)
        .map(|line| format!("{line}:     let value_{line} = compute_value({line});\n"))
        .collect::<String>();

    let suggestion_keys = vec!["exec.source-read-dedupe-v1".to_string()];
    let compacted = compact_output_for_suggestions(
        &[
            "bash".to_string(),
            "-lc".to_string(),
            "sed -n '10,259p' src/lib.rs".to_string(),
        ],
        output.as_str(),
        &suggestion_keys,
    )
    .expect("accepted source read dedupe should compact");

    assert_eq!(compacted.filter_id, "exec.source-read-dedupe-v1");
    assert!(compacted.text.contains("omitted 250 lines from src/lib.rs"));
    assert!(compacted.text.contains("requested lines 10-259"));
    assert!(!compacted.text.contains("value_259"));
    assert!(compacted.compacted_token_count < compacted.original_token_count);
}

#[test]
fn small_output_returns_none() {
    assert_eq!(
        compact_output(&["cargo test".to_string()], "test result: ok. 1 passed"),
        None
    );
}

#[test]
fn unknown_command_returns_none() {
    let output = "plain output line\n".repeat(1000);
    assert_eq!(
        compact_output(&["cat file.txt".to_string()], output.as_str()),
        None
    );
}

#![expect(clippy::expect_used)]

//! Optional smoke tests that hit the real OpenAI /v1/responses endpoint. They run with the rest of
//! the core suite but self-skip unless `OPENAI_API_KEY` is set in the local environment.

use assert_cmd::prelude::*;
use predicates::prelude::*;
use std::process::Command;
use tempfile::TempDir;

const OPENAI_API_KEY_ENV_VAR: &str = "OPENAI_API_KEY";

fn skip_if_missing_openai_api_key(test_name: &str) -> Option<String> {
    let api_key = std::env::var(OPENAI_API_KEY_ENV_VAR)
        .ok()
        .filter(|value| !value.trim().is_empty());
    if api_key.is_none() {
        eprintln!("skipping {test_name} - {OPENAI_API_KEY_ENV_VAR} not set");
    }
    api_key
}

/// Helper that spawns the binary inside a TempDir with minimal flags. Returns (Assert, TempDir).
fn run_live(prompt: &str, openai_api_key: &str) -> (assert_cmd::assert::Assert, TempDir) {
    #![expect(clippy::unwrap_used)]
    let dir = TempDir::new().unwrap();

    let mut cmd = Command::new(codex_utils_cargo_bin::cargo_bin("codex").unwrap());
    cmd.env(OPENAI_API_KEY_ENV_VAR, openai_api_key)
        .arg("exec")
        .arg("--skip-git-repo-check")
        .arg("--dangerously-bypass-approvals-and-sandbox")
        .arg("--cd")
        .arg(dir.path())
        .arg(prompt);

    let output = cmd.output().expect("failed to run codex exec");
    (output.assert(), dir)
}

#[test]
fn live_create_file_hello_txt() {
    let Some(openai_api_key) = skip_if_missing_openai_api_key("live_create_file_hello_txt") else {
        return;
    };

    let (assert, dir) = run_live(
        "Use the shell tool with the apply_patch command to create a file named hello.txt containing the text 'hello'.",
        &openai_api_key,
    );

    assert.success();

    let path = dir.path().join("hello.txt");
    assert!(path.exists(), "hello.txt was not created by the model");

    let contents = std::fs::read_to_string(path).unwrap();

    assert_eq!(contents.trim(), "hello");
}

#[test]
fn live_print_working_directory() {
    let Some(openai_api_key) = skip_if_missing_openai_api_key("live_print_working_directory")
    else {
        return;
    };

    let (assert, dir) = run_live(
        "Print the current working directory using the shell function.",
        &openai_api_key,
    );

    assert
        .success()
        .stdout(predicate::str::contains(dir.path().to_string_lossy()));
}

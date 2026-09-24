use std::fs;
use std::path::Path;

use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;

use super::*;

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(ToString::to_string).collect()
}

#[test]
fn explicit_flags_override_base_and_repeats_form_arrays() {
    let input = workflow_invocation_input_from_args(
        Path::new("/work"),
        &strings(&[
            "--output",
            "markdown",
            "--input",
            r#"{"output":"json","workingDirectory":"/custom"}"#,
            "--tag",
            "one",
            "--tag=two",
        ]),
    )
    .expect("normalize input");

    assert_eq!(
        input,
        json!({
            "output": "markdown",
            "workingDirectory": "/custom",
            "tag": ["one", "two"],
        })
    );
}

#[test]
fn repeated_array_values_remain_distinct_occurrences() {
    let input = workflow_invocation_input_from_args(
        Path::new("/work"),
        &strings(&["--tag", "[1,2]", "--tag", "[3,4]"]),
    )
    .expect("normalize input");

    assert_eq!(
        input,
        json!({
            "tag": [[1, 2], [3, 4]],
            "workingDirectory": "/work",
        })
    );
}

#[test]
fn reads_bounded_input_file_relative_to_invocation_directory() {
    let temp = TempDir::new().expect("tempdir");
    fs::write(temp.path().join("input.json"), r#"{"count":2}"#).expect("write input");

    assert_eq!(
        workflow_invocation_input_from_args(temp.path(), &strings(&["--input", "@input.json"]))
            .expect("read input"),
        json!({
            "count": 2,
            "workingDirectory": temp.path().to_string_lossy(),
        })
    );
}

#[test]
fn rejects_input_files_over_the_hard_byte_limit() {
    let temp = TempDir::new().expect("tempdir");
    fs::write(temp.path().join("large.json"), vec![b' '; 1024 * 1024 + 1])
        .expect("write oversized input");

    let error =
        workflow_invocation_input_from_args(temp.path(), &strings(&["--input", "@large.json"]))
            .expect_err("reject oversized input");
    assert!(error.message().contains("exceeds 1048576 bytes"));
}

#[test]
fn rejects_positional_and_non_kebab_case_arguments() {
    let positional =
        workflow_invocation_input(Path::new("/work"), "old input").expect_err("reject positional");
    assert!(positional.message().contains("no longer supported"));

    let invalid = workflow_invocation_input(Path::new("/work"), "--not_snake value")
        .expect_err("reject underscore");
    assert!(invalid.message().contains("kebab-case"));
}

#[test]
fn rejects_non_object_base_input() {
    let error =
        workflow_invocation_input(Path::new("/work"), "--input '[1,2]'").expect_err("reject array");
    assert_eq!(
        error.message(),
        "Invalid workflow arguments: --input must be a JSON object."
    );
}

#[test]
fn injects_a_foreign_platform_working_directory_without_host_path_conversion() {
    assert_eq!(
        normalize_workflow_input_with_working_directory(
            r"C:\workspace\project",
            json!({}),
            /*flags*/ serde_json::Map::new(),
        )
        .expect("normalize foreign working directory"),
        json!({ "workingDirectory": r"C:\workspace\project" })
    );
}

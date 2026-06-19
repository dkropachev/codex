use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

fn write_workflow(root: &Path, dirname: &str, yaml: &str) -> PathBuf {
    let workflow_dir = root.join(dirname);
    fs::create_dir_all(&workflow_dir).expect("create workflow dir");
    fs::write(workflow_dir.join("workflow.yaml"), yaml).expect("write workflow yaml");
    workflow_dir
}

#[test]
fn discovers_home_and_project_workflow_commands() {
    let temp = tempfile::tempdir().expect("tempdir");
    let codex_home = temp.path().join("home");
    let cwd = temp.path().join("project");
    let home_dir = write_workflow(
        &codex_home.join("workflows"),
        "code-review",
        r#"
id: code-review
command: code-review
title: /code-review
userDescription: Run a code review workflow.
"#,
    );
    let project_dir = write_workflow(
        &cwd.join(".codex").join("workflows"),
        "report",
        r#"
id: report
command: report
userDescription: Build a project report.
"#,
    );

    let commands = discover_workflow_commands(&codex_home, &cwd);

    assert_eq!(
        commands,
        vec![
            WorkflowCommand {
                id: "code-review".to_string(),
                command: "code-review".to_string(),
                description: "Run a code review workflow.".to_string(),
                option_hints: Vec::new(),
                workflow_dir: home_dir,
            },
            WorkflowCommand {
                id: "report".to_string(),
                command: "report".to_string(),
                description: "Build a project report.".to_string(),
                option_hints: Vec::new(),
                workflow_dir: project_dir,
            },
        ]
    );
}

#[test]
fn project_workflow_overrides_home_command_name() {
    let temp = tempfile::tempdir().expect("tempdir");
    let codex_home = temp.path().join("home");
    let cwd = temp.path().join("project");
    write_workflow(
        &codex_home.join("workflows"),
        "review",
        "command: review\nuserDescription: Home review\n",
    );
    let project_dir = write_workflow(
        &cwd.join(".codex").join("workflows"),
        "review",
        "command: review\nuserDescription: Project review\n",
    );

    let commands = discover_workflow_commands(&codex_home, &cwd);

    assert_eq!(
        commands,
        vec![WorkflowCommand {
            id: "review".to_string(),
            command: "review".to_string(),
            description: "Project review".to_string(),
            option_hints: Vec::new(),
            workflow_dir: project_dir,
        }]
    );
}

#[test]
fn discovers_nested_workflow_ids() {
    let temp = tempfile::tempdir().expect("tempdir");
    let codex_home = temp.path().join("home");
    let cwd = temp.path().join("project");
    let workflow_dir = write_workflow(
        &codex_home.join("workflows").join("review"),
        "fix",
        "id: review/fix\ncommand: code-review\nuserDescription: Review fix\n",
    );

    let commands = discover_workflow_commands(&codex_home, &cwd);

    assert_eq!(
        commands,
        vec![WorkflowCommand {
            id: "review/fix".to_string(),
            command: "code-review".to_string(),
            description: "Review fix".to_string(),
            option_hints: Vec::new(),
            workflow_dir,
        }]
    );
}

#[test]
fn discovers_workflow_usage_option_hints() {
    let temp = tempfile::tempdir().expect("tempdir");
    let codex_home = temp.path().join("home");
    let cwd = temp.path().join("project");
    let workflow_dir = write_workflow(
        &codex_home.join("workflows"),
        "code-review",
        r#"
id: code-review
command: code-review
userDescription: Run review
usage:
  options:
    - flag: --action
      valueHint: <review|list-reports>
      description: Run mode.
    - flag: --include-preexisting
      description: Keep preexisting findings.
    - --output <json|md>
"#,
    );

    let commands = discover_workflow_commands(&codex_home, &cwd);

    assert_eq!(
        commands,
        vec![WorkflowCommand {
            id: "code-review".to_string(),
            command: "code-review".to_string(),
            description: "Run review".to_string(),
            option_hints: vec![
                WorkflowCommandOptionHint {
                    display: "--action <review|list-reports>".to_string(),
                    description: Some("Run mode.".to_string()),
                },
                WorkflowCommandOptionHint {
                    display: "--include-preexisting".to_string(),
                    description: Some("Keep preexisting findings.".to_string()),
                },
                WorkflowCommandOptionHint {
                    display: "--output <json|md>".to_string(),
                    description: None,
                },
            ],
            workflow_dir,
        }]
    );
}

#[test]
fn ignores_missing_or_invalid_command_names() {
    let temp = tempfile::tempdir().expect("tempdir");
    let codex_home = temp.path().join("home");
    let cwd = temp.path().join("project");
    write_workflow(
        &codex_home.join("workflows"),
        "missing-command",
        "id: missing-command\nuserDescription: Missing command\n",
    );
    write_workflow(
        &codex_home.join("workflows"),
        "slash-command",
        "command: /bad\nuserDescription: Bad command\n",
    );
    write_workflow(
        &codex_home.join("workflows"),
        "upper-command",
        "command: Bad\nuserDescription: Bad command\n",
    );

    assert_eq!(discover_workflow_commands(&codex_home, &cwd), Vec::new());
}

#[test]
fn parses_workflow_args_into_input_json() {
    let cwd = Path::new("/tmp/project");

    let input = workflow_invocation_input(
        cwd,
        r#"--action list-reports --output=md --include-skipped-by-limit --allowed-areas tui --allowed-areas core --max-count 3 --dry-run false"#,
    )
    .expect("input");

    assert_eq!(
        input,
        json!({
            "action": "list-reports",
            "output": "md",
            "includeSkippedByLimit": true,
            "allowedAreas": ["tui", "core"],
            "maxCount": 3,
            "dryRun": false,
            "workingDirectory": "/tmp/project",
        })
    );
}

#[test]
fn merges_input_object_and_flags_without_overriding_working_directory() {
    let cwd = Path::new("/tmp/project");

    let input = workflow_invocation_input(
        cwd,
        r#"--input '{"action":"report","workingDirectory":"/custom"}' --output json"#,
    )
    .expect("input");

    assert_eq!(
        input,
        json!({
            "action": "report",
            "workingDirectory": "/custom",
            "output": "json",
        })
    );
}

#[test]
fn rejects_malformed_workflow_args() {
    let cwd = Path::new("/tmp/project");

    let err = workflow_invocation_input(cwd, "--input '{bad}'").expect_err("invalid JSON");
    assert_eq!(
        err.message(),
        "Invalid workflow arguments: --input is not valid JSON: key must be a string at line 1 column 2"
    );

    let err = workflow_invocation_input(cwd, "positional").expect_err("positional");
    assert_eq!(
        err.message(),
        "Invalid workflow arguments: unexpected positional argument 'positional'. Use --name value pairs."
    );

    let err = workflow_invocation_input(cwd, "--bad-").expect_err("bad flag");
    assert_eq!(
        err.message(),
        "Invalid workflow arguments: invalid flag name '--bad-'."
    );
}

#[test]
fn builds_shell_command_for_workflow_directory() {
    let command = WorkflowCommand {
        id: "code-review".to_string(),
        command: "code-review".to_string(),
        description: "Run review".to_string(),
        option_hints: Vec::new(),
        workflow_dir: PathBuf::from("/tmp/codex home/workflows/code-review"),
    };

    let shell_command =
        build_workflow_shell_command(&command, Path::new("/tmp/project"), "--action list-reports")
            .expect("shell command");

    assert_eq!(
        shell_command,
        r#"cd '/tmp/codex home/workflows/code-review' && bun src/workflow.ts --input '{"action":"list-reports","workingDirectory":"/tmp/project"}'"#
    );
}

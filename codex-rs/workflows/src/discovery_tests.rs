use std::fs;
use std::path::Path;

use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::*;

fn write_manifest(root: &Path, relative: &str, contents: &str) -> PathBuf {
    let path = root.join(relative);
    fs::create_dir_all(&path).expect("create workflow directory");
    fs::write(path.join("workflow.yaml"), contents).expect("write workflow metadata");
    path
}

#[test]
fn discovers_canonical_and_legacy_packages_with_project_precedence() {
    let temp = TempDir::new().expect("tempdir");
    let home = temp.path().join("home");
    let cwd = temp.path().join("project");
    write_manifest(
        &home.join("workflows"),
        "review",
        "id: review\ncommand: review\nuserDescription: legacy\n",
    );
    let project = write_manifest(
        &cwd.join(".codex/workflows"),
        "review",
        "apiVersion: 1\nid: review\ntitle: Review\ncallableName: review\ndescription: canonical\nvalidation:\n  commands: []\n  coverage:\n    positive: true\n    load: true\n    autocomplete: true\n    negative: true\n",
    );

    assert_eq!(
        discover_workflow_commands(&home, &cwd),
        vec![WorkflowCommand {
            id: "review".to_string(),
            command: "review".to_string(),
            description: "canonical".to_string(),
            option_hints: Vec::new(),
            workflow_dir: project,
        }]
    );
}

#[test]
fn ignores_symlinked_workflow_directories() {
    let temp = TempDir::new().expect("tempdir");
    let home = temp.path().join("home");
    let cwd = temp.path().join("project");
    fs::create_dir_all(home.join("workflows")).expect("create root");
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let outside = write_manifest(temp.path(), "outside", "id: outside\ncommand: outside\n");
        symlink(outside, home.join("workflows/linked")).expect("create symlink");
        assert_eq!(discover_workflow_commands(&home, &cwd), Vec::new());
    }
}

#[cfg(unix)]
#[test]
fn ignores_symlinked_workflow_metadata_files() {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new().expect("tempdir");
    let home = temp.path().join("home");
    let cwd = temp.path().join("project");
    let workflow = home.join("workflows/linked-metadata");
    fs::create_dir_all(&workflow).expect("create workflow directory");
    let metadata = temp.path().join("outside.yaml");
    fs::write(
        &metadata,
        "id: linked\ncommand: linked\nuserDescription: linked metadata\n",
    )
    .expect("write outside metadata");
    symlink(metadata, workflow.join("workflow.yaml")).expect("link workflow metadata");

    assert_eq!(discover_workflow_commands(&home, &cwd), Vec::new());
}

#[cfg(unix)]
#[test]
fn follows_a_symlinked_configured_workflow_root() {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new().expect("tempdir");
    let home = temp.path().join("home");
    let cwd = temp.path().join("project");
    let outside = temp.path().join("outside-root");
    let package = write_manifest(
        &outside,
        "linked",
        "id: linked\ncommand: linked\nuserDescription: linked root\n",
    );
    fs::create_dir_all(&home).expect("create home");
    symlink(&outside, home.join("workflows")).expect("link workflow root");

    let commands = discover_workflow_commands(&home, &cwd);
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].id, "linked");
    assert_eq!(commands[0].workflow_dir, home.join("workflows/linked"));
    assert_eq!(package.file_name(), commands[0].workflow_dir.file_name());
}

#[test]
fn discovers_a_package_nested_beneath_another_package() {
    let temp = TempDir::new().expect("tempdir");
    let home = temp.path().join("home");
    let cwd = temp.path().join("project");
    let root = home.join("workflows");
    write_manifest(
        &root,
        "parent",
        "id: parent\ncommand: parent\nuserDescription: parent\n",
    );
    write_manifest(
        &root,
        "parent/child",
        "id: parent/child\ncommand: child\nuserDescription: child\n",
    );

    assert_eq!(
        discover_workflow_commands(&home, &cwd)
            .into_iter()
            .map(|command| command.id)
            .collect::<Vec<_>>(),
        vec!["parent".to_string(), "parent/child".to_string()]
    );
}

#[test]
fn rejects_unsafe_manifest_ids_without_hiding_safe_legacy_ids() {
    let temp = TempDir::new().expect("tempdir");
    let home = temp.path().join("home");
    let cwd = temp.path().join("project");
    let root = home.join("workflows");
    write_manifest(
        &root,
        "unsafe",
        "id: ../escape\ncommand: unsafe\nuserDescription: unsafe\n",
    );
    write_manifest(
        &root,
        "legacy-uppercase",
        "id: Legacy/Review\ncommand: legacy-review\nuserDescription: legacy\n",
    );
    write_manifest(
        &root,
        "canonical-uppercase",
        "apiVersion: 1\nid: Canonical\ntitle: Invalid\ncallableName: invalid\ndescription: invalid\nvalidation:\n  commands: []\n  coverage:\n    positive: true\n    load: true\n    autocomplete: true\n    negative: true\n",
    );

    let commands = discover_workflow_commands(&home, &cwd);
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].id, "Legacy/Review");
}

#[test]
fn ignores_windows_reserved_canonical_ids_without_hiding_legacy_packages() {
    let temp = TempDir::new().expect("tempdir");
    let home = temp.path().join("home");
    let cwd = temp.path().join("project");
    let root = home.join("workflows");
    write_manifest(
        &root,
        "canonical-reserved",
        "apiVersion: 1\nid: reports/con\ntitle: Invalid\ncallableName: invalid\ndescription: invalid\nvalidation:\n  commands: []\n  coverage:\n    positive: true\n    load: true\n    autocomplete: true\n    negative: true\n",
    );
    let legacy = write_manifest(
        &root,
        "legacy-reserved",
        "id: con\ncommand: legacy-con\nuserDescription: legacy\n",
    );

    assert_eq!(
        discover_workflow_commands(&home, &cwd),
        vec![WorkflowCommand {
            id: "con".to_string(),
            command: "legacy-con".to_string(),
            description: "legacy".to_string(),
            option_hints: Vec::new(),
            workflow_dir: legacy,
        }]
    );
}

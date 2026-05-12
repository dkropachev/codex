use std::fs;
use std::path::PathBuf;

use anyhow::Context as _;
use anyhow::Result;

use crate::id::parse_mention_target;
use crate::spec::WORKFLOW_YAML;
use crate::spec::read_workflow_spec;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowContext {
    pub id: String,
    pub title: Option<String>,
    pub user_description: Option<String>,
    pub root_path: PathBuf,
    pub path: PathBuf,
    pub workflow_yaml: String,
    pub readme: Option<String>,
}

pub fn read_workflow_context_from_mention_target(target: &str) -> Result<WorkflowContext> {
    let target = parse_mention_target(target)?;
    let workflow_yaml_path = target.path.join(WORKFLOW_YAML);
    let workflow_yaml = fs::read_to_string(&workflow_yaml_path).with_context(|| {
        format!(
            "failed to read workflow metadata {}",
            workflow_yaml_path.display()
        )
    })?;
    let spec = read_workflow_spec(&workflow_yaml_path)?;
    let readme_path = target.path.join("README.md");
    let readme = fs::read_to_string(&readme_path).ok();
    Ok(WorkflowContext {
        id: target.id,
        title: spec.title,
        user_description: spec.user_description,
        root_path: target.root_path,
        path: target.path,
        workflow_yaml,
        readme,
    })
}

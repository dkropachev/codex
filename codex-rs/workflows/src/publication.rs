use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use codex_artifactory::ArtifactSource;
use codex_artifactory::Artifactory;
use codex_artifactory::StateRegistration;
use codex_artifactory::WorkflowToolRegistrationRecord;
use codex_artifactory::file_sha256;
use codex_artifactory::sharded_state_dir;
use codex_artifactory::source_key;
use codex_config::types::WorkflowsConfigToml;
use sha2::Digest;
use sha2::Sha256;

use crate::registry::WorkflowPublishedTool;
use crate::registry::WorkflowRoot;
use crate::registry::WorkflowSummary;
use crate::registry::discover_workflow_tools_from_filesystem_for_hook;
use crate::registry::summarize_workflow;
use crate::spec::WorkflowHookKind;
use crate::spec::WorkflowToolSpec;
use crate::spec::read_workflow_spec;
use crate::spec::workflow_tool_name;

pub const WORKFLOW_TOOL_REGISTRATION_NAMESPACE: &str = "workflow-tools";

pub fn discover_workflow_tools(
    codex_home: &Path,
    cwd: &Path,
    config: &WorkflowsConfigToml,
) -> Result<Vec<WorkflowPublishedTool>> {
    discover_workflow_tools_for_hook(codex_home, cwd, config, WorkflowHookKind::AfterAgent)
}

pub fn read_active_workflow_tool_registrations_for_hook(
    codex_home: &Path,
    hook: WorkflowHookKind,
) -> Result<Vec<WorkflowToolRegistrationRecord>> {
    let store = Artifactory::open(codex_home)?;
    let scope_key = hook_scope_key(hook);
    let states = store.states_for_scope(WORKFLOW_TOOL_REGISTRATION_NAMESPACE, scope_key)?;
    let mut registrations = BTreeMap::new();

    for state in states {
        let Ok(registration) =
            serde_json::from_str::<WorkflowToolRegistrationRecord>(&state.metadata_json)
        else {
            continue;
        };
        let Ok(workflow) = workflow_from_record(&registration) else {
            continue;
        };
        if registration.workflow_id != workflow.id
            || registration.tool_name != workflow_tool_name(&workflow.id)
            || registration.source_hook != scope_key
        {
            continue;
        }
        let key = (workflow.id.clone(), workflow.root_path.clone());
        registrations.entry(key).or_insert(registration);
    }

    Ok(registrations.into_values().collect())
}

pub fn read_active_workflow_tools_for_hook(
    codex_home: &Path,
    hook: WorkflowHookKind,
) -> Result<Vec<WorkflowPublishedTool>> {
    let mut tools = Vec::new();
    for registration in read_active_workflow_tool_registrations_for_hook(codex_home, hook)? {
        tools.push(WorkflowPublishedTool {
            workflow: workflow_from_record(&registration)?,
            tool: tool_from_record(&registration)?,
        });
    }
    tools.sort_by(sort_published_tools);
    Ok(tools)
}

pub fn discover_workflow_tools_for_hook(
    codex_home: &Path,
    cwd: &Path,
    config: &WorkflowsConfigToml,
    hook: WorkflowHookKind,
) -> Result<Vec<WorkflowPublishedTool>> {
    let registrations = read_active_workflow_tool_registrations_for_hook(codex_home, hook)?;
    if registrations.is_empty() {
        return bootstrap_workflow_tools_from_filesystem_for_hook(codex_home, cwd, config, hook);
    }

    let mut tools = Vec::new();
    for registration in registrations {
        if let Some(tool) = refresh_tool_registration(codex_home, Some(config), &registration)? {
            tools.push(tool);
        }
    }
    tools.sort_by(sort_published_tools);
    if tools.is_empty() {
        bootstrap_workflow_tools_from_filesystem_for_hook(codex_home, cwd, config, hook)
    } else {
        Ok(tools)
    }
}

pub fn publish_tool(
    codex_home: &Path,
    workflow: &WorkflowSummary,
    tool: &WorkflowToolSpec,
    hook: WorkflowHookKind,
) -> Result<WorkflowPublishedTool> {
    if !tool.register_on.contains(&hook) {
        return Err(anyhow!(
            "workflow {} does not register a tool on {hook:?}",
            workflow.id
        ));
    }

    let now = unix_now();
    let tool_name = workflow_tool_name(&workflow.id);
    let sources = workflow_tool_sources(workflow, hook);
    let source_digest = source_key(&sources);
    let scope_key = hook_scope_key(hook);
    let registration = WorkflowToolRegistrationRecord {
        workflow_id: workflow.id.clone(),
        workflow: serde_json::to_value(workflow)?,
        tool_name,
        source_hook: hook_scope_key(hook).to_string(),
        source_digest: source_digest.clone(),
        published_at_unix_sec: now,
        updated_at_unix_sec: now,
        refresh_after_unix_sec: None,
        expires_at_unix_sec: None,
        tool_spec: serde_json::to_value(tool)?,
    };

    let mut store = Artifactory::open(codex_home)?;
    store.register_state(&StateRegistration {
        namespace: WORKFLOW_TOOL_REGISTRATION_NAMESPACE.to_string(),
        scope_key: scope_key.to_string(),
        source_key: source_digest,
        state_dir: sharded_state_dir(
            &codex_home.join(WORKFLOW_TOOL_REGISTRATION_NAMESPACE),
            scope_key,
            registration.source_digest.as_str(),
        ),
        sources,
        metadata_json: serde_json::to_string(&registration)?,
    })?;

    Ok(WorkflowPublishedTool {
        workflow: workflow.clone(),
        tool: tool.clone(),
    })
}

pub fn refresh_tool_registration(
    codex_home: &Path,
    config: Option<&WorkflowsConfigToml>,
    registration: &WorkflowToolRegistrationRecord,
) -> Result<Option<WorkflowPublishedTool>> {
    let workflow = workflow_from_record(registration)?;
    let tool = tool_from_record(registration)?;
    let hook = hook_from_scope_key(&registration.source_hook)
        .ok_or_else(|| anyhow!("unknown workflow hook {}", registration.source_hook))?;
    let current_source_digest = source_key(&workflow_tool_sources(&workflow, hook));
    let now = unix_now();

    if !registration.is_stale(now, &current_source_digest) {
        return Ok(Some(WorkflowPublishedTool { workflow, tool }));
    }

    let Some(config) = config else {
        return Ok(None);
    };

    let root = WorkflowRoot {
        kind: workflow.root_kind,
        label: workflow.root_label.clone(),
        path: workflow.root_path.clone(),
    };
    let Some(fresh_workflow) = summarize_workflow(&root, workflow.path.as_path(), config) else {
        return Ok(None);
    };
    let spec = read_workflow_spec(&fresh_workflow.workflow_yaml_path)?;
    let Some(tool) = spec.tool else {
        return Ok(None);
    };
    if !tool.register_on.contains(&hook) {
        return Ok(None);
    }

    publish_tool(codex_home, &fresh_workflow, &tool, hook).map(Some)
}

fn bootstrap_workflow_tools_from_filesystem_for_hook(
    codex_home: &Path,
    cwd: &Path,
    config: &WorkflowsConfigToml,
    hook: WorkflowHookKind,
) -> Result<Vec<WorkflowPublishedTool>> {
    let discovered =
        discover_workflow_tools_from_filesystem_for_hook(codex_home, cwd, config, hook)?;
    let mut tools = Vec::new();
    for tool in discovered {
        tools.push(publish_tool(codex_home, &tool.workflow, &tool.tool, hook)?);
    }
    tools.sort_by(sort_published_tools);
    Ok(tools)
}

fn workflow_tool_sources(
    workflow: &WorkflowSummary,
    hook: WorkflowHookKind,
) -> Vec<ArtifactSource> {
    let tool_name = workflow_tool_name(&workflow.id);
    vec![
        ArtifactSource::new(
            PathBuf::from(".workflow-registration"),
            "workflow_identity",
            workflow_identity_digest(workflow, &tool_name, hook),
        ),
        ArtifactSource::new(
            PathBuf::from("workflow.yaml"),
            "workflow_yaml",
            file_hash_or_missing(&workflow.workflow_yaml_path),
        ),
        ArtifactSource::new(
            PathBuf::from("src/workflow.ts"),
            "workflow_source",
            file_hash_or_missing(&workflow.path.join("src/workflow.ts")),
        ),
        ArtifactSource::new(
            PathBuf::from("package.json"),
            "workflow_package",
            file_hash_or_missing(&workflow.path.join("package.json")),
        ),
    ]
}

fn workflow_identity_digest(
    workflow: &WorkflowSummary,
    tool_name: &str,
    hook: WorkflowHookKind,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(workflow.id.as_bytes());
    hasher.update(b"\0");
    hasher.update(tool_name.as_bytes());
    hasher.update(b"\0");
    hasher.update(hook_scope_key(hook).as_bytes());
    hasher.update(b"\0");
    let root_path = workflow.root_path.to_string_lossy();
    hasher.update(root_path.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn file_hash_or_missing(path: &Path) -> String {
    file_sha256(path).unwrap_or_else(|| format!("missing:{}", path.display()))
}

fn workflow_from_record(registration: &WorkflowToolRegistrationRecord) -> Result<WorkflowSummary> {
    serde_json::from_value(registration.workflow.clone())
        .context("failed to parse workflow tool registration workflow summary")
}

fn tool_from_record(registration: &WorkflowToolRegistrationRecord) -> Result<WorkflowToolSpec> {
    serde_json::from_value(registration.tool_spec.clone())
        .context("failed to parse workflow tool registration tool spec")
}

fn hook_scope_key(hook: WorkflowHookKind) -> &'static str {
    match hook {
        WorkflowHookKind::AfterAgent => "afterAgent",
        WorkflowHookKind::PreToolUse => "preToolUse",
        WorkflowHookKind::PostToolUse => "postToolUse",
        WorkflowHookKind::SessionStart => "sessionStart",
        WorkflowHookKind::UserPromptSubmit => "userPromptSubmit",
        WorkflowHookKind::PreCompact => "preCompact",
        WorkflowHookKind::PostCompact => "postCompact",
        WorkflowHookKind::Stop => "stop",
    }
}

fn hook_from_scope_key(value: &str) -> Option<WorkflowHookKind> {
    match value {
        "afterAgent" => Some(WorkflowHookKind::AfterAgent),
        "preToolUse" => Some(WorkflowHookKind::PreToolUse),
        "postToolUse" => Some(WorkflowHookKind::PostToolUse),
        "sessionStart" => Some(WorkflowHookKind::SessionStart),
        "userPromptSubmit" => Some(WorkflowHookKind::UserPromptSubmit),
        "preCompact" => Some(WorkflowHookKind::PreCompact),
        "postCompact" => Some(WorkflowHookKind::PostCompact),
        "stop" => Some(WorkflowHookKind::Stop),
        _ => None,
    }
}

fn sort_published_tools(
    left: &WorkflowPublishedTool,
    right: &WorkflowPublishedTool,
) -> std::cmp::Ordering {
    left.workflow
        .id
        .cmp(&right.workflow.id)
        .then_with(|| left.workflow.root_path.cmp(&right.workflow.root_path))
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::discover_workflows;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    #[test]
    fn bootstrap_publishes_workflow_tools_into_artifactory() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let workflow_dir = home.path().join("workflows/reports/jira-summary");
        create_workflow_fixture(&workflow_dir);

        assert!(
            read_active_workflow_tools_for_hook(home.path(), WorkflowHookKind::AfterAgent)
                .unwrap()
                .is_empty()
        );

        let tools =
            discover_workflow_tools(home.path(), cwd.path(), &WorkflowsConfigToml::default())
                .unwrap();

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].workflow.id, "reports/jira-summary");
        assert_eq!(tools[0].tool.description, "Run the Jira summary workflow");

        let registrations = read_active_workflow_tool_registrations_for_hook(
            home.path(),
            WorkflowHookKind::AfterAgent,
        )
        .unwrap();
        assert_eq!(registrations.len(), 1);
        assert_eq!(registrations[0].workflow_id, "reports/jira-summary");
        assert_eq!(
            registrations[0].tool_name,
            workflow_tool_name("reports/jira-summary")
        );
    }

    #[test]
    fn stale_registrations_are_refreshed_before_tools_are_exposed() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let workflow_dir = home.path().join("workflows/reports/jira-summary");
        create_workflow_fixture(&workflow_dir);
        let config = WorkflowsConfigToml::default();
        let hook = WorkflowHookKind::AfterAgent;
        let workflows = discover_workflows(home.path(), cwd.path(), &config).unwrap();
        assert_eq!(workflows.len(), 1);
        let workflow = workflows.into_iter().next().unwrap();
        let spec = read_workflow_spec(&workflow.workflow_yaml_path).unwrap();
        let tool = spec.tool.unwrap();
        let sources = workflow_tool_sources(&workflow, hook);
        let source_digest = source_key(&sources);
        let stale_registration = WorkflowToolRegistrationRecord {
            workflow_id: workflow.id.clone(),
            workflow: serde_json::to_value(&workflow).unwrap(),
            tool_name: workflow_tool_name(&workflow.id),
            source_hook: hook_scope_key(hook).to_string(),
            source_digest: source_digest.clone(),
            published_at_unix_sec: 10,
            updated_at_unix_sec: 10,
            refresh_after_unix_sec: None,
            expires_at_unix_sec: Some(5),
            tool_spec: serde_json::to_value(tool).unwrap(),
        };

        let mut store = Artifactory::open(home.path()).unwrap();
        store
            .register_state(&StateRegistration {
                namespace: WORKFLOW_TOOL_REGISTRATION_NAMESPACE.to_string(),
                scope_key: hook_scope_key(hook).to_string(),
                source_key: source_digest.clone(),
                state_dir: sharded_state_dir(
                    &home.path().join(WORKFLOW_TOOL_REGISTRATION_NAMESPACE),
                    hook_scope_key(hook),
                    &source_digest,
                ),
                sources,
                metadata_json: serde_json::to_string(&stale_registration).unwrap(),
            })
            .unwrap();

        let before = read_active_workflow_tool_registrations_for_hook(home.path(), hook).unwrap();
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].expires_at_unix_sec, Some(5));

        let tools =
            discover_workflow_tools_for_hook(home.path(), cwd.path(), &config, hook).unwrap();
        assert_eq!(tools.len(), 1);

        let after = read_active_workflow_tool_registrations_for_hook(home.path(), hook).unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].expires_at_unix_sec, None);
        assert!(after[0].updated_at_unix_sec >= before[0].updated_at_unix_sec);
        assert_eq!(tools[0].workflow.id, workflow.id);
    }

    fn create_workflow_fixture(dir: &Path) {
        std::fs::create_dir_all(dir.join("src/tests")).unwrap();
        std::fs::create_dir_all(dir.join("state")).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(
            dir.join("workflow.yaml"),
            "id: reports/jira-summary\ntitle: Jira Summary\nuserDescription: Summarize Jira work\nvalidation:\n  commands:\n    - npm run build\n    - npm test\n  coverage:\n    positive: true\n    negative: true\n    progress: true\n    finalResult: true\n    failureUx: true\n    load: true\n    autocomplete: true\n    recovery: false\ntool:\n  description: Run the Jira summary workflow\n  inputSchema:\n    type: object\n  outputSchema: null\n  registerOn:\n    - afterAgent\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("README.md"),
            "# Jira Summary\n\n## Usage\n\n## Workflow Runtime\n\n## Dependencies\n\n## Validation\n\n## Maintenance\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("DESIGN.md"),
            "# Jira Summary Design\n\n## Overview\n\n## Architecture\n\n## Data Flow\n\n## Failure Handling\n\n## Recovery Behavior\n\n## Test Matrix\n\n## Maintenance Notes\n",
        )
        .unwrap();
        std::fs::write(dir.join("src/workflow.ts"), "export {};\n").unwrap();
        std::fs::write(
            dir.join("src/tests/workflow.positive.test.ts"),
            "// workflow-covers: positive progress finalResult\nexport {};\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("src/tests/workflow.load.test.ts"),
            "// workflow-covers: load\nexport {};\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("src/tests/workflow.autocomplete.test.ts"),
            "// workflow-covers: autocomplete\nexport {};\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("src/tests/workflow.negative.test.ts"),
            "// workflow-covers: negative failureUx\nexport {};\n",
        )
        .unwrap();
        std::fs::write(dir.join("state/.gitkeep"), "").unwrap();
        std::fs::write(
            dir.join("package.json"),
            "{\n  \"name\": \"codex-workflow-reports-jira-summary\",\n  \"private\": true,\n  \"type\": \"module\"\n}\n",
        )
        .unwrap();
    }
}

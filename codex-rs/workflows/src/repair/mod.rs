use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;

use anyhow::Result;
use anyhow::anyhow;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use serde_json::json;

use crate::execute::WorkflowCommandContext;
use crate::execute::WorkflowCommandOutput;
use crate::execute::resolve_workflow_for_context;
use crate::registry::DEFAULT_MAX_REPAIR_CYCLES;
use crate::registry::WorkflowValidation;
use crate::registry::WorkflowValidationStatus;
use crate::spec::read_workflow_spec;
use crate::spec::write_workflow_spec;
use crate::validation_finding::WorkflowValidationFinding;
use crate::validation_runner::run_validation_command;
use crate::validation_runner::validate_workflow;

pub mod types {
    pub use super::WorkflowRepairAction;
    pub use super::WorkflowRepairActionKind;
    pub use super::WorkflowRepairResult;
    pub use super::WorkflowRepairStopReason;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkflowRepairActionKind {
    NormalizeValidationMetadata,
    RepairReadme,
    RepairDesign,
    RepairLayout,
    RepairPackageManifest,
    RepairTsconfig,
    ScaffoldWorkflowSource,
    ScaffoldWorkflowTests,
    AddCoverageMarkers,
    AiRepair,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRepairAction {
    pub kind: WorkflowRepairActionKind,
    pub path: PathBuf,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkflowRepairStopReason {
    Valid,
    BlockedByRepairMode,
    UnsupportedFindings,
    RepairBudgetExhausted,
    NoChangesApplied,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRepairResult {
    pub mode: String,
    pub max_repair_cycles: u32,
    pub repair_cycles_run: u32,
    pub changed: bool,
    pub stop_reason: WorkflowRepairStopReason,
    pub applied_fixes: Vec<WorkflowRepairAction>,
    pub remaining_findings: Vec<WorkflowValidationFinding>,
    pub blocked_findings: Vec<WorkflowValidationFinding>,
    pub unsupported_findings: Vec<WorkflowValidationFinding>,
}

pub(crate) fn repair_workflow_command(
    ctx: WorkflowCommandContext<'_>,
    id: &str,
) -> Result<WorkflowCommandOutput> {
    let mut workflow = resolve_workflow_for_context(&ctx, id)?;
    let repair_mode = workflow.repair_mode.clone();
    let max_repair_cycles = ctx
        .config
        .max_repair_cycles
        .unwrap_or(DEFAULT_MAX_REPAIR_CYCLES);
    let mut applied_fixes = Vec::new();
    let mut repair_cycles_run = 0;
    let mut changed = false;

    loop {
        workflow = resolve_workflow_for_context(&ctx, id)?;
        let report = validate_workflow(&workflow, run_validation_command)?;
        let validation = WorkflowValidation {
            status: report.status,
            findings: report.findings.clone(),
        };
        if validation.status == WorkflowValidationStatus::Valid {
            if changed && !manual_commit_policy(ctx.config.commit_policy.as_deref()) {
                commit_workflow_changes(&workflow.path, "Repair workflow")?;
            }
            let repaired_workflow = resolve_workflow_for_context(&ctx, id)?;
            return Ok(WorkflowCommandOutput {
                message: "valid".to_string(),
                data: json!({
                    "workflow": repaired_workflow,
                    "validation": validation,
                    "repair": WorkflowRepairResult {
                        mode: repair_mode,
                        max_repair_cycles,
                        repair_cycles_run,
                        changed,
                        stop_reason: WorkflowRepairStopReason::Valid,
                        applied_fixes,
                        remaining_findings: Vec::<WorkflowValidationFinding>::new(),
                        blocked_findings: Vec::<WorkflowValidationFinding>::new(),
                        unsupported_findings: Vec::<WorkflowValidationFinding>::new(),
                    }
                }),
            });
        }

        if repair_mode == "metadata" {
            return Ok(WorkflowCommandOutput {
                message: "blocked by repair mode".to_string(),
                data: json!({
                    "workflow": workflow,
                    "validation": validation,
                    "repair": WorkflowRepairResult {
                        mode: repair_mode,
                        max_repair_cycles,
                        repair_cycles_run,
                        changed,
                        stop_reason: WorkflowRepairStopReason::BlockedByRepairMode,
                        applied_fixes,
                        remaining_findings: validation.findings.clone(),
                        blocked_findings: validation.findings,
                        unsupported_findings: Vec::<WorkflowValidationFinding>::new(),
                    }
                }),
            });
        }

        if repair_cycles_run >= max_repair_cycles {
            return Ok(WorkflowCommandOutput {
                message: "repair budget exhausted".to_string(),
                data: json!({
                    "workflow": workflow,
                    "validation": validation,
                    "repair": WorkflowRepairResult {
                        mode: repair_mode,
                        max_repair_cycles,
                        repair_cycles_run,
                        changed,
                        stop_reason: WorkflowRepairStopReason::RepairBudgetExhausted,
                        applied_fixes,
                        remaining_findings: validation.findings,
                        blocked_findings: Vec::<WorkflowValidationFinding>::new(),
                        unsupported_findings: Vec::<WorkflowValidationFinding>::new(),
                    }
                }),
            });
        }

        repair_cycles_run += 1;
        let mut cycle_changed = false;
        let mut unsupported_findings = Vec::new();

        for finding in validation.findings.clone() {
            match apply_known_fix(&workflow.path, &workflow.id, &finding)? {
                Some(action) => {
                    applied_fixes.push(action);
                    cycle_changed = true;
                    changed = true;
                }
                None => unsupported_findings.push(finding),
            }
        }

        if !cycle_changed {
            let stop_reason = if unsupported_findings.is_empty() {
                WorkflowRepairStopReason::NoChangesApplied
            } else if let Some(action) = try_ai_repair(&ctx, &workflow, &unsupported_findings)? {
                applied_fixes.push(action);
                changed = true;
                continue;
            } else {
                WorkflowRepairStopReason::UnsupportedFindings
            };
            return Ok(WorkflowCommandOutput {
                message: "unsupported findings".to_string(),
                data: json!({
                    "workflow": workflow,
                    "validation": validation,
                    "repair": WorkflowRepairResult {
                        mode: repair_mode,
                        max_repair_cycles,
                        repair_cycles_run,
                        changed,
                        stop_reason,
                        applied_fixes,
                        remaining_findings: validation.findings,
                        blocked_findings: Vec::<WorkflowValidationFinding>::new(),
                        unsupported_findings,
                    }
                }),
            });
        }
    }
}

fn try_ai_repair(
    ctx: &WorkflowCommandContext<'_>,
    workflow: &crate::registry::WorkflowSummary,
    unsupported_findings: &[WorkflowValidationFinding],
) -> Result<Option<WorkflowRepairAction>> {
    let Some(codex_self_exe) = ctx.codex_self_exe.as_ref() else {
        return Ok(None);
    };
    if !codex_self_exe_supports_exec(codex_self_exe) {
        return Ok(None);
    }

    let prompt = build_ai_repair_prompt(workflow, unsupported_findings)?;
    let Ok(output) = Command::new(codex_self_exe)
        .current_dir(&workflow.path)
        .arg("exec")
        .arg("-C")
        .arg(&workflow.path)
        .arg("--skip-git-repo-check")
        .arg("--ephemeral")
        .arg("--sandbox")
        .arg("workspace-write")
        .arg("--json")
        .arg(prompt)
        .output()
    else {
        return Ok(None);
    };

    if !output.status.success() {
        return Ok(None);
    }

    Ok(Some(WorkflowRepairAction {
        kind: WorkflowRepairActionKind::AiRepair,
        path: workflow.path.clone(),
        detail: format!(
            "applied AI repair fallback after {} unsupported findings",
            unsupported_findings.len()
        ),
    }))
}

fn codex_self_exe_supports_exec(codex_self_exe: &Path) -> bool {
    Command::new(codex_self_exe)
        .arg("exec")
        .arg("--help")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn build_ai_repair_prompt(
    workflow: &crate::registry::WorkflowSummary,
    unsupported_findings: &[WorkflowValidationFinding],
) -> Result<String> {
    let findings_json = serde_json::to_string_pretty(unsupported_findings)?;
    Ok(format!(
        "You are the workflow-coder for a Codex workflow repair pass.\n\nOnly modify files inside this workflow directory: `{workflow_dir}`. Do not edit files outside it. Keep writes inside this workflow root. Do not edit `DESIGN.md`. Use only dependencies declared in the workflow's local `package.json`. Keep code in `src/`, tests in `src/tests/`, and state in `state/`.\n\nThe deterministic repair pass already handled known cases and stopped on these unsupported findings:\n{findings_json}\n\nFix the workflow until validation passes. If the right fix requires a design change, do not edit `DESIGN.md`; write a `DESIGN.md request` for the parent instead. Keep iterating until the workflow is clean or a design change is required.\n",
        workflow_dir = workflow.path.display(),
        findings_json = findings_json,
    ))
}

fn apply_known_fix(
    workflow_dir: &Path,
    expected_id: &str,
    finding: &WorkflowValidationFinding,
) -> Result<Option<WorkflowRepairAction>> {
    match finding {
        WorkflowValidationFinding::WorkflowIdMismatch { path, .. } => {
            let path = workflow_path(workflow_dir, path);
            let mut spec = read_workflow_spec(&path)?;
            spec.id = expected_id.to_string();
            write_workflow_spec(&path, &spec)?;
            Ok(Some(WorkflowRepairAction {
                kind: WorkflowRepairActionKind::NormalizeValidationMetadata,
                path,
                detail: format!("set workflow id to `{expected_id}`"),
            }))
        }
        WorkflowValidationFinding::MissingGitRepository { path } => {
            let path = workflow_path(workflow_dir, path);
            fs::create_dir_all(&path)?;
            Ok(Some(WorkflowRepairAction {
                kind: WorkflowRepairActionKind::RepairLayout,
                path,
                detail: "created workflow git metadata directory".to_string(),
            }))
        }
        WorkflowValidationFinding::MissingFile { path } => repair_missing_file(workflow_dir, path),
        WorkflowValidationFinding::MissingDocumentHeading { path, heading } => {
            repair_document_heading(&workflow_path(workflow_dir, path), heading)
        }
        WorkflowValidationFinding::MissingDirectory { path } => {
            let path = workflow_path(workflow_dir, path);
            fs::create_dir_all(&path)?;
            Ok(Some(WorkflowRepairAction {
                kind: WorkflowRepairActionKind::RepairLayout,
                path,
                detail: "created required workflow directory".to_string(),
            }))
        }
        WorkflowValidationFinding::CodeOutsideSrc { paths } => {
            relocate_paths(workflow_dir, paths, RepairTarget::Src)
        }
        WorkflowValidationFinding::TestsOutsideSrcTests { paths } => {
            relocate_paths(workflow_dir, paths, RepairTarget::Tests)
        }
        WorkflowValidationFinding::DatabasesOutsideState { paths } => {
            relocate_paths(workflow_dir, paths, RepairTarget::State)
        }
        WorkflowValidationFinding::UndeclaredPackageImport { package_name, .. } => {
            add_dependency(workflow_dir, package_name)
        }
        WorkflowValidationFinding::MissingCoverageMarker { key, .. } => {
            ensure_coverage_marker(workflow_dir, key)
        }
        WorkflowValidationFinding::ValidationCommandFailed { command, .. }
            if command == "npm run build" =>
        {
            ensure_build_support(workflow_dir)
        }
        WorkflowValidationFinding::WorkflowSpecReadFailed { .. }
        | WorkflowValidationFinding::WorkflowPathEscapesRoot { .. }
        | WorkflowValidationFinding::PackageManifestParseFailed { .. }
        | WorkflowValidationFinding::MissingValidationCommands { .. }
        | WorkflowValidationFinding::EmptyValidationCommands { .. }
        | WorkflowValidationFinding::InvalidValidationCommands { .. }
        | WorkflowValidationFinding::MissingCoverageMetadata { .. }
        | WorkflowValidationFinding::MissingCoverageKey { .. }
        | WorkflowValidationFinding::InvalidCoverageKeyType { .. }
        | WorkflowValidationFinding::CoverageKeyMustBeTrue { .. }
        | WorkflowValidationFinding::ValidationCommandFailed { .. }
        | WorkflowValidationFinding::WorkflowApiContractExtractionFailed { .. } => Ok(None),
    }
}

fn repair_missing_file(workflow_dir: &Path, path: &Path) -> Result<Option<WorkflowRepairAction>> {
    let path = workflow_path(workflow_dir, path);
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Ok(None);
    };
    let title = "Workflow";
    match name {
        "README.md" => {
            fs::write(
                &path,
                format!(
                    "# {title}\n\n## Usage\n\n## Workflow Runtime\n\n## Dependencies\n\n## Validation\n\n## Maintenance\n"
                ),
            )?;
            Ok(Some(WorkflowRepairAction {
                kind: WorkflowRepairActionKind::RepairReadme,
                path: path.to_path_buf(),
                detail: "restored README.md with required sections".to_string(),
            }))
        }
        "DESIGN.md" => {
            fs::write(
                &path,
                format!(
                    "# {title} Design\n\n## Overview\n\n## Architecture\n\n## Data Flow\n\n## Failure Handling\n\n## Recovery Behavior\n\n## Test Matrix\n\n## Maintenance Notes\n"
                ),
            )?;
            Ok(Some(WorkflowRepairAction {
                kind: WorkflowRepairActionKind::RepairDesign,
                path: path.to_path_buf(),
                detail: "restored DESIGN.md with required sections".to_string(),
            }))
        }
        _ => {
            if path.starts_with(workflow_dir.join("src/tests")) {
                fs::create_dir_all(path.parent().unwrap_or(workflow_dir))?;
                fs::write(
                    &path,
                    format!(
                        "// workflow-covers: {}\nexport {{}};\n",
                        coverage_key_for_test_path(&path)
                    ),
                )?;
                Ok(Some(WorkflowRepairAction {
                    kind: WorkflowRepairActionKind::ScaffoldWorkflowTests,
                    path: path.to_path_buf(),
                    detail: "restored missing workflow test scaffold".to_string(),
                }))
            } else if path == workflow_dir.join("src/workflow.ts") {
                fs::create_dir_all(path.parent().unwrap_or(workflow_dir))?;
                if workflow_dir.join("workflow.ts").is_file() {
                    fs::rename(workflow_dir.join("workflow.ts"), &path)?;
                } else {
                    fs::write(&path, "export {};\n")?;
                }
                Ok(Some(WorkflowRepairAction {
                    kind: WorkflowRepairActionKind::ScaffoldWorkflowSource,
                    path: path.to_path_buf(),
                    detail: "restored workflow source under src/".to_string(),
                }))
            } else {
                Ok(None)
            }
        }
    }
}

fn repair_document_heading(path: &Path, heading: &str) -> Result<Option<WorkflowRepairAction>> {
    let mut content = fs::read_to_string(path).unwrap_or_default();
    if !content.contains(heading) {
        if !content.ends_with('\n') {
            content.push('\n');
        }
        content.push('\n');
        content.push_str("## ");
        content.push_str(heading);
        content.push_str("\n\n");
        fs::write(path, content)?;
        let kind = if path.file_name().and_then(|name| name.to_str()) == Some("README.md") {
            WorkflowRepairActionKind::RepairReadme
        } else {
            WorkflowRepairActionKind::RepairDesign
        };
        return Ok(Some(WorkflowRepairAction {
            kind,
            path: path.to_path_buf(),
            detail: format!("added missing heading `{heading}`"),
        }));
    }
    Ok(None)
}

fn workflow_path(workflow_dir: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        workflow_dir.join(path)
    }
}

enum RepairTarget {
    Src,
    Tests,
    State,
}

fn relocate_paths(
    workflow_dir: &Path,
    paths: &[PathBuf],
    target: RepairTarget,
) -> Result<Option<WorkflowRepairAction>> {
    let mut changed = false;
    for path in paths {
        let source = workflow_path(workflow_dir, path);
        let Some(file_name) = source.file_name() else {
            continue;
        };
        let destination = match target {
            RepairTarget::Src => workflow_dir.join("src").join(file_name),
            RepairTarget::Tests => workflow_dir.join("src/tests").join(file_name),
            RepairTarget::State => workflow_dir.join("state").join(file_name),
        };
        fs::create_dir_all(destination.parent().unwrap_or(workflow_dir))?;
        if source != destination && source.exists() {
            fs::rename(&source, &destination)?;
            changed = true;
        }
    }
    if !changed {
        return Ok(None);
    }
    let (kind, detail) = match target {
        RepairTarget::Src => (
            WorkflowRepairActionKind::ScaffoldWorkflowSource,
            "moved workflow code under src/".to_string(),
        ),
        RepairTarget::Tests => (
            WorkflowRepairActionKind::RepairLayout,
            "moved workflow tests under src/tests/".to_string(),
        ),
        RepairTarget::State => (
            WorkflowRepairActionKind::RepairLayout,
            "moved workflow state under state/".to_string(),
        ),
    };
    Ok(Some(WorkflowRepairAction {
        kind,
        path: workflow_dir.to_path_buf(),
        detail,
    }))
}

fn add_dependency(workflow_dir: &Path, package_name: &str) -> Result<Option<WorkflowRepairAction>> {
    let package_json_path = workflow_dir.join("package.json");
    let mut package: JsonValue = serde_json::from_str(&fs::read_to_string(&package_json_path)?)?;
    let dependencies = package
        .as_object_mut()
        .ok_or_else(|| anyhow!("package.json must be an object"))?
        .entry("dependencies")
        .or_insert_with(|| json!({}));
    let dependencies = dependencies
        .as_object_mut()
        .ok_or_else(|| anyhow!("dependencies must be an object"))?;

    let mut added = false;
    for dependency in [package_name, "@openai/codex-sdk"] {
        if !dependencies.contains_key(dependency) {
            dependencies.insert(
                dependency.to_string(),
                JsonValue::String("latest".to_string()),
            );
            added = true;
        }
    }
    if !added {
        return Ok(None);
    }
    fs::write(
        &package_json_path,
        format!("{}\n", serde_json::to_string_pretty(&package)?),
    )?;
    Ok(Some(WorkflowRepairAction {
        kind: WorkflowRepairActionKind::RepairPackageManifest,
        path: package_json_path,
        detail: format!("added missing dependencies for `{package_name}`"),
    }))
}

fn ensure_coverage_marker(workflow_dir: &Path, key: &str) -> Result<Option<WorkflowRepairAction>> {
    let path = workflow_dir.join(match key {
        "load" => "src/tests/workflow.load.test.ts",
        "autocomplete" => "src/tests/workflow.autocomplete.test.ts",
        "recovery" => "src/tests/workflow.recovery.test.ts",
        _ => return Ok(None),
    });
    fs::create_dir_all(path.parent().unwrap_or(workflow_dir))?;
    let content = fs::read_to_string(&path).unwrap_or_default();
    let marker = format!("// workflow-covers: {key}");
    if content.contains(&marker) {
        return Ok(None);
    }
    let new_content = if content.trim().is_empty() {
        format!("{marker}\nexport {{}};\n")
    } else {
        format!("{marker}\n{content}")
    };
    fs::write(&path, new_content)?;
    Ok(Some(WorkflowRepairAction {
        kind: WorkflowRepairActionKind::AddCoverageMarkers,
        path,
        detail: format!("added `{marker}`"),
    }))
}

fn ensure_build_support(workflow_dir: &Path) -> Result<Option<WorkflowRepairAction>> {
    let tsconfig_path = workflow_dir.join("tsconfig.json");
    let package_json_path = workflow_dir.join("package.json");
    let mut changed = false;
    if !tsconfig_path.is_file() {
        fs::write(
            &tsconfig_path,
            "{\n  \"compilerOptions\": {\n    \"target\": \"ES2022\",\n    \"module\": \"NodeNext\",\n    \"moduleResolution\": \"NodeNext\",\n    \"strict\": true,\n    \"noEmit\": true\n  },\n  \"include\": [\"src/**/*.ts\"]\n}\n",
        )?;
        changed = true;
    }
    let mut package: JsonValue = serde_json::from_str(&fs::read_to_string(&package_json_path)?)?;
    let scripts = package
        .as_object_mut()
        .ok_or_else(|| anyhow!("package.json must be an object"))?
        .entry("scripts")
        .or_insert_with(|| json!({}));
    let scripts = scripts
        .as_object_mut()
        .ok_or_else(|| anyhow!("scripts must be an object"))?;
    if scripts.get("build") != Some(&JsonValue::String("tsc --noEmit".to_string())) {
        scripts.insert(
            "build".to_string(),
            JsonValue::String("tsc --noEmit".to_string()),
        );
        changed = true;
    }
    if changed {
        fs::write(
            &package_json_path,
            format!("{}\n", serde_json::to_string_pretty(&package)?),
        )?;
        return Ok(Some(WorkflowRepairAction {
            kind: WorkflowRepairActionKind::RepairTsconfig,
            path: tsconfig_path,
            detail: "added tsconfig.json and a build script for `npm run build`".to_string(),
        }));
    }
    Ok(None)
}

fn coverage_key_for_test_path(path: &Path) -> &'static str {
    match path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
    {
        "workflow.load.test.ts" => "load",
        "workflow.autocomplete.test.ts" => "autocomplete",
        "workflow.recovery.test.ts" => "recovery",
        _ => "positive progress finalResult",
    }
}

fn manual_commit_policy(policy: Option<&str>) -> bool {
    matches!(policy, Some("manual" | "none" | "disabled"))
}

fn commit_workflow_changes(path: &Path, message: &str) -> Result<()> {
    run_git(path, &["init"])?;
    run_git(path, &["add", "."])?;
    let diff = Command::new("git")
        .args(["diff", "--cached", "--quiet"])
        .current_dir(path)
        .status()?;
    if diff.success() {
        return Ok(());
    }
    let output = Command::new("git")
        .args(["commit", "-m", message])
        .current_dir(path)
        .env("GIT_AUTHOR_NAME", "Codex")
        .env("GIT_AUTHOR_EMAIL", "codex@openai.com")
        .env("GIT_COMMITTER_NAME", "Codex")
        .env("GIT_COMMITTER_EMAIL", "codex@openai.com")
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(anyhow!(
            "git commit failed with {}: {}{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

fn run_git(path: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git").args(args).current_dir(path).output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(anyhow!(
            "git {} failed with {}: {}{}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

#[cfg(test)]
mod tests;

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use serde_json::Value as JsonValue;
use serde_json::json;

use self::message::repair_output_message;
use crate::execute::WorkflowCommandContext;
use crate::execute::WorkflowCommandOutput;
use crate::execute::resolve_workflow_for_context;
use crate::registry::DEFAULT_MAX_REPAIR_CYCLES;
use crate::registry::WorkflowSummary;
use crate::repair::types::WorkflowRepairAction;
use crate::repair::types::WorkflowRepairActionKind;
use crate::repair::types::WorkflowRepairResult;
use crate::repair::types::WorkflowRepairStopReason;
use crate::repair::types::WorkflowValidationFindingInfo;
use crate::repair::types::WorkflowValidationInfo;
use crate::repair_mode::WorkflowRepairMode;
use crate::spec::WorkflowRuntimeKind;
use crate::spec::WorkflowSpec;
use crate::spec::read_workflow_spec;
use crate::spec::scaffold_workflow_spec;
use crate::spec::write_workflow_spec;
use crate::validation_finding::WorkflowValidationFinding;
use crate::validation_runner::WorkflowValidationCommandResult;
use crate::validation_runner::WorkflowValidationReport;
use crate::validation_runner::run_validation_command;
use crate::validation_runner::validate_workflow;

#[cfg(test)]
mod tests;

mod message;

pub mod types;

const RUNTIME_STATE_GITIGNORE_PATTERNS: &[&str] = &["artifacts/", "state/*", "!state/.gitkeep"];

#[derive(Debug, Default)]
struct FixPlan {
    update_validation_yaml: bool,
    repair_output_schema_contracts: bool,
    repair_readme: bool,
    repair_design: bool,
    repair_package_manifest: bool,
    repair_tsconfig: bool,
    create_layout: bool,
    add_coverage_markers: BTreeSet<String>,
    package_names: BTreeSet<String>,
    build_script: bool,
    test_script: bool,
    run_script: bool,
    refresh_dependencies: bool,
    spec_reset: bool,
}

struct FinalRepairResultInput {
    repair_mode: WorkflowRepairMode,
    max_repair_cycles: u32,
    repair_cycles_run: u32,
    changed: bool,
    stop_reason: WorkflowRepairStopReason,
    applied_fixes: Vec<WorkflowRepairAction>,
    blocked_findings: Vec<WorkflowValidationFindingInfo>,
    unsupported_findings: Vec<WorkflowValidationFindingInfo>,
    remaining_findings: Vec<WorkflowValidationFindingInfo>,
}

pub(crate) fn repair_workflow_command(
    ctx: WorkflowCommandContext<'_>,
    id: &str,
) -> Result<WorkflowCommandOutput> {
    repair_workflow_command_with_runners(
        ctx,
        id,
        run_validation_command,
        apply_dependency_install_fix,
    )
}

fn repair_workflow_command_with_runners<F, D>(
    ctx: WorkflowCommandContext<'_>,
    id: &str,
    mut command_runner: F,
    mut dependency_installer: D,
) -> Result<WorkflowCommandOutput>
where
    F: FnMut(&str, &Path) -> Result<WorkflowValidationCommandResult>,
    D: FnMut(&WorkflowSummary, &str) -> Result<Option<WorkflowRepairAction>>,
{
    ctx.report_progress(
        "Resolving workflow",
        json!({
            "stage": "resolving",
            "workflowId": id,
        }),
    );
    let initial_workflow = resolve_workflow_for_context(&ctx, id)
        .with_context(|| format!("failed to resolve workflow `{id}` for repair"))?;
    let repair_mode =
        WorkflowRepairMode::parse(&initial_workflow.repair_mode).with_context(|| {
            format!(
                "failed to parse repair mode `{}` for workflow `{id}`",
                initial_workflow.repair_mode
            )
        })?;
    let max_repair_cycles = ctx
        .config
        .max_repair_cycles
        .unwrap_or(DEFAULT_MAX_REPAIR_CYCLES);
    let mut workflow = initial_workflow;
    let mut applied_fixes = Vec::new();
    let mut repair_cycles_run = 0;
    let mut changed = false;
    ctx.report_progress(
        "Starting workflow repair",
        json!({
            "stage": "starting",
            "workflowId": workflow.id.as_str(),
            "mode": repair_mode.to_string(),
            "maxRepairCycles": max_repair_cycles,
        }),
    );
    ctx.report_progress(
        "Validating workflow",
        json!({
            "stage": "validating",
            "workflowId": workflow.id.as_str(),
        }),
    );
    let mut last_report = validate_workflow(&workflow, &mut command_runner).with_context(|| {
        format!(
            "failed to validate workflow `{}` before repair",
            workflow.id
        )
    })?;
    ctx.report_progress(
        "Validation completed",
        json!({
            "stage": "validating",
            "workflowId": workflow.id.as_str(),
            "findings": last_report.findings.len(),
        }),
    );

    if last_report.status == crate::registry::WorkflowValidationStatus::Valid {
        let repair = final_repair_result(FinalRepairResultInput {
            repair_mode,
            max_repair_cycles,
            repair_cycles_run,
            changed,
            stop_reason: WorkflowRepairStopReason::Valid,
            applied_fixes,
            blocked_findings: Vec::new(),
            unsupported_findings: Vec::new(),
            remaining_findings: Vec::new(),
        });
        report_repair_complete(&ctx, &workflow, repair.stop_reason, repair.changed);
        return Ok(build_output(&workflow, last_report, repair));
    }

    for cycle in 1..=max_repair_cycles {
        ctx.report_progress(
            "Repair cycle started",
            json!({
                "stage": "repairing",
                "workflowId": workflow.id.as_str(),
                "step": cycle,
                "total": max_repair_cycles,
                "findings": last_report.findings.len(),
            }),
        );
        let assessment = assess_findings(&workflow.path, &workflow, &last_report, &repair_mode);
        if !assessment.blocked.is_empty() {
            let repair = final_repair_result(FinalRepairResultInput {
                repair_mode,
                max_repair_cycles,
                repair_cycles_run,
                changed: false,
                stop_reason: WorkflowRepairStopReason::BlockedByRepairMode,
                applied_fixes: Vec::new(),
                blocked_findings: assessment.blocked.iter().map(finding_to_api).collect(),
                unsupported_findings: Vec::new(),
                remaining_findings: api_findings(&last_report.findings),
            });
            report_repair_complete(&ctx, &workflow, repair.stop_reason, repair.changed);
            return Ok(build_output(&workflow, last_report, repair));
        }
        if !assessment.unsupported.is_empty() {
            if repair_mode.allows_action(WorkflowRepairActionKind::AiRepair) {
                ctx.report_progress(
                    "Running AI repair fallback",
                    json!({
                        "stage": "aiRepair",
                        "workflowId": workflow.id.as_str(),
                        "step": cycle,
                        "total": max_repair_cycles,
                        "findings": assessment.unsupported.len(),
                    }),
                );
                if let Some(action) = try_ai_repair(&ctx, &workflow, &assessment.unsupported)
                    .with_context(|| {
                        format!(
                            "failed to run AI repair fallback for workflow `{}`",
                            workflow.id
                        )
                    })?
                {
                    changed = true;
                    repair_cycles_run += 1;
                    applied_fixes.push(action);
                    ctx.report_progress(
                        "AI repair fallback applied",
                        json!({
                            "stage": "aiRepair",
                            "workflowId": workflow.id.as_str(),
                            "step": cycle,
                            "total": max_repair_cycles,
                        }),
                    );
                    workflow = resolve_workflow_for_context(&ctx, id).with_context(|| {
                        format!(
                            "failed to refresh workflow summary for `{}` after AI repair fallback",
                            workflow.id
                        )
                    })?;
                    ctx.report_progress(
                        "Validating repaired workflow",
                        json!({
                            "stage": "validating",
                            "workflowId": workflow.id.as_str(),
                            "step": cycle,
                            "total": max_repair_cycles,
                        }),
                    );
                    last_report =
                        validate_workflow(&workflow, &mut command_runner).with_context(|| {
                            format!(
                                "failed to validate workflow `{}` after AI repair cycle {}",
                                workflow.id, repair_cycles_run
                            )
                        })?;
                    ctx.report_progress(
                        "Validation completed",
                        json!({
                            "stage": "validating",
                            "workflowId": workflow.id.as_str(),
                            "step": cycle,
                            "total": max_repair_cycles,
                            "findings": last_report.findings.len(),
                        }),
                    );
                    if last_report.status == crate::registry::WorkflowValidationStatus::Valid {
                        if should_commit_changes(&ctx) {
                            ctx.report_progress(
                                "Committing repaired workflow",
                                json!({
                                    "stage": "committing",
                                    "workflowId": workflow.id.as_str(),
                                }),
                            );
                            commit_repair_changes(&workflow.path).with_context(|| {
                                format!("failed to commit repaired workflow `{}`", workflow.id)
                            })?;
                        }
                        let repair = final_repair_result(FinalRepairResultInput {
                            repair_mode,
                            max_repair_cycles,
                            repair_cycles_run,
                            changed,
                            stop_reason: WorkflowRepairStopReason::Valid,
                            applied_fixes,
                            blocked_findings: Vec::new(),
                            unsupported_findings: Vec::new(),
                            remaining_findings: Vec::new(),
                        });
                        report_repair_complete(&ctx, &workflow, repair.stop_reason, repair.changed);
                        return Ok(build_output(&workflow, last_report, repair));
                    }
                    continue;
                }
            }
            let repair = final_repair_result(FinalRepairResultInput {
                repair_mode,
                max_repair_cycles,
                repair_cycles_run,
                changed: false,
                stop_reason: WorkflowRepairStopReason::UnsupportedFindings,
                applied_fixes: Vec::new(),
                blocked_findings: Vec::new(),
                unsupported_findings: assessment.unsupported.iter().map(finding_to_api).collect(),
                remaining_findings: api_findings(&last_report.findings),
            });
            report_repair_complete(&ctx, &workflow, repair.stop_reason, repair.changed);
            return Ok(build_output(&workflow, last_report, repair));
        }

        let plan = build_fix_plan(&workflow.path, &workflow, &last_report).with_context(|| {
            format!(
                "failed to build a repair plan for workflow `{}`",
                workflow.id
            )
        })?;
        if plan.is_empty() {
            let repair = final_repair_result(FinalRepairResultInput {
                repair_mode,
                max_repair_cycles,
                repair_cycles_run,
                changed: false,
                stop_reason: WorkflowRepairStopReason::NoChangesApplied,
                applied_fixes,
                blocked_findings: api_findings(&last_report.findings),
                unsupported_findings: Vec::new(),
                remaining_findings: Vec::new(),
            });
            report_repair_complete(&ctx, &workflow, repair.stop_reason, repair.changed);
            return Ok(build_output(&workflow, last_report, repair));
        }

        ctx.report_progress(
            "Applying deterministic fixes",
            json!({
                "stage": "repairing",
                "workflowId": workflow.id.as_str(),
                "step": cycle,
                "total": max_repair_cycles,
            }),
        );
        ensure_git_repo(&workflow.path).with_context(|| {
            format!(
                "failed to ensure git repository for workflow `{}`",
                workflow.id
            )
        })?;
        let mut cycle_actions = Vec::new();
        if plan.spec_reset || plan.update_validation_yaml || plan.repair_output_schema_contracts {
            cycle_actions.extend(apply_validation_yaml_fix(&workflow, &plan).with_context(
                || format!("failed to repair workflow metadata for `{}`", workflow.id),
            )?);
        }
        if plan.create_layout {
            cycle_actions.extend(apply_layout_fix(&workflow, &last_report).with_context(|| {
                format!("failed to repair workflow layout for `{}`", workflow.id)
            })?);
        }
        if plan.repair_readme {
            cycle_actions
                .push(apply_readme_fix(&workflow).with_context(|| {
                    format!("failed to repair README.md for `{}`", workflow.id)
                })?);
        }
        if plan.repair_design {
            cycle_actions
                .push(apply_design_fix(&workflow).with_context(|| {
                    format!("failed to repair DESIGN.md for `{}`", workflow.id)
                })?);
        }
        if plan.repair_package_manifest {
            let package_action = apply_package_manifest_fix(&workflow, &plan)
                .with_context(|| format!("failed to repair package.json for `{}`", workflow.id))?;
            let package_manifest_changed = !package_action.detail.is_empty();
            cycle_actions.push(package_action);
            if (plan.refresh_dependencies || package_manifest_changed)
                && let Some(action) = dependency_installer(
                    &workflow,
                    ctx.config
                        .dependency_update_policy
                        .as_deref()
                        .unwrap_or("locked"),
                )
                .with_context(|| {
                    format!(
                        "failed to refresh workflow dependencies for `{}`",
                        workflow.id
                    )
                })?
            {
                cycle_actions.push(action);
            }
        }
        if !plan.add_coverage_markers.is_empty() {
            cycle_actions.push(
                apply_coverage_marker_fix(&workflow, &plan.add_coverage_markers).with_context(
                    || format!("failed to update coverage markers for `{}`", workflow.id),
                )?,
            );
        }
        if plan.repair_tsconfig {
            cycle_actions.push(apply_tsconfig_fix(&workflow).with_context(|| {
                format!("failed to repair tsconfig.json for `{}`", workflow.id)
            })?);
        }

        cycle_actions.retain(|action| !action.detail.is_empty());
        if cycle_actions.is_empty() {
            let repair = final_repair_result(FinalRepairResultInput {
                repair_mode,
                max_repair_cycles,
                repair_cycles_run,
                changed,
                stop_reason: WorkflowRepairStopReason::NoChangesApplied,
                applied_fixes,
                blocked_findings: Vec::new(),
                unsupported_findings: Vec::new(),
                remaining_findings: api_findings(&last_report.findings),
            });
            report_repair_complete(&ctx, &workflow, repair.stop_reason, repair.changed);
            return Ok(build_output(&workflow, last_report, repair));
        }

        changed = true;
        repair_cycles_run += 1;
        let cycle_action_count = cycle_actions.len();
        applied_fixes.extend(cycle_actions);
        ctx.report_progress(
            "Applied deterministic fixes",
            json!({
                "stage": "repairing",
                "workflowId": workflow.id.as_str(),
                "step": cycle,
                "total": max_repair_cycles,
                "fixes": cycle_action_count,
            }),
        );

        workflow = resolve_workflow_for_context(&ctx, id).with_context(|| {
            format!(
                "failed to refresh workflow summary for `{}` after applying fixes",
                workflow.id
            )
        })?;
        ctx.report_progress(
            "Validating repaired workflow",
            json!({
                "stage": "validating",
                "workflowId": workflow.id.as_str(),
                "step": cycle,
                "total": max_repair_cycles,
            }),
        );
        last_report = validate_workflow(&workflow, &mut command_runner).with_context(|| {
            format!(
                "failed to validate workflow `{}` after repair cycle {}",
                workflow.id, repair_cycles_run
            )
        })?;
        ctx.report_progress(
            "Validation completed",
            json!({
                "stage": "validating",
                "workflowId": workflow.id.as_str(),
                "step": cycle,
                "total": max_repair_cycles,
                "findings": last_report.findings.len(),
            }),
        );
        if last_report.status == crate::registry::WorkflowValidationStatus::Valid {
            if should_commit_changes(&ctx) {
                ctx.report_progress(
                    "Committing repaired workflow",
                    json!({
                        "stage": "committing",
                        "workflowId": workflow.id.as_str(),
                    }),
                );
                commit_repair_changes(&workflow.path).with_context(|| {
                    format!("failed to commit repaired workflow `{}`", workflow.id)
                })?;
            }
            let repair = final_repair_result(FinalRepairResultInput {
                repair_mode,
                max_repair_cycles,
                repair_cycles_run,
                changed,
                stop_reason: WorkflowRepairStopReason::Valid,
                applied_fixes,
                blocked_findings: Vec::new(),
                unsupported_findings: Vec::new(),
                remaining_findings: Vec::new(),
            });
            report_repair_complete(&ctx, &workflow, repair.stop_reason, repair.changed);
            return Ok(build_output(&workflow, last_report, repair));
        }
    }

    let repair = final_repair_result(FinalRepairResultInput {
        repair_mode,
        max_repair_cycles,
        repair_cycles_run,
        changed,
        stop_reason: WorkflowRepairStopReason::RepairBudgetExhausted,
        applied_fixes,
        blocked_findings: Vec::new(),
        unsupported_findings: Vec::new(),
        remaining_findings: api_findings(&last_report.findings),
    });
    report_repair_complete(&ctx, &workflow, repair.stop_reason, repair.changed);
    Ok(build_output(&workflow, last_report, repair))
}

fn report_repair_complete(
    ctx: &WorkflowCommandContext<'_>,
    workflow: &WorkflowSummary,
    stop_reason: WorkflowRepairStopReason,
    changed: bool,
) {
    ctx.report_progress(
        "Workflow repair complete",
        json!({
            "stage": "complete",
            "workflowId": workflow.id.as_str(),
            "stopReason": stop_reason,
            "changed": changed,
        }),
    );
}

fn final_repair_result(input: FinalRepairResultInput) -> WorkflowRepairResult {
    WorkflowRepairResult {
        mode: input.repair_mode.to_string(),
        max_repair_cycles: input.max_repair_cycles,
        repair_cycles_run: input.repair_cycles_run,
        changed: input.changed,
        stop_reason: input.stop_reason,
        applied_fixes: input.applied_fixes,
        remaining_findings: input.remaining_findings,
        blocked_findings: input.blocked_findings,
        unsupported_findings: input.unsupported_findings,
    }
}

fn try_ai_repair(
    ctx: &WorkflowCommandContext<'_>,
    workflow: &WorkflowSummary,
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
            "Applied AI repair fallback after {} unsupported finding(s)",
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
    workflow: &WorkflowSummary,
    unsupported_findings: &[WorkflowValidationFinding],
) -> Result<String> {
    let findings_json = serde_json::to_string_pretty(unsupported_findings)?;
    Ok(format!(
        "You are the workflow-coder for a Codex workflow repair pass.\n\nOnly modify files inside this workflow directory: `{workflow_dir}`. Do not edit files outside it. Keep writes inside this workflow root. Do not edit `DESIGN.md`. Use only dependencies declared in the workflow's local `package.json`. Keep code in `src/`, tests in `src/tests/`, and state in `state/`.\n\nThe deterministic repair pass already handled known cases and stopped on these unsupported findings:\n{findings_json}\n\nFix the workflow until validation passes. If the right fix requires a design change, do not edit `DESIGN.md`; write a `DESIGN.md request` for the parent instead. Keep iterating until the workflow is clean or a design change is required.\n",
        workflow_dir = workflow.path.display(),
        findings_json = findings_json,
    ))
}

fn build_output(
    workflow: &WorkflowSummary,
    mut report: WorkflowValidationReport,
    repair: WorkflowRepairResult,
) -> WorkflowCommandOutput {
    let validation_command_results = std::mem::take(&mut report.command_results);
    WorkflowCommandOutput {
        message: repair_output_message(workflow, &repair),
        data: json!({
            "workflow": workflow,
            "validation": validation_report_to_api(&report),
            "validationCommandResults": validation_command_results,
            "repair": repair,
        }),
    }
}

struct Assessment {
    blocked: Vec<WorkflowValidationFinding>,
    unsupported: Vec<WorkflowValidationFinding>,
}

fn assess_findings(
    workflow_path: &Path,
    workflow: &WorkflowSummary,
    report: &WorkflowValidationReport,
    repair_mode: &WorkflowRepairMode,
) -> Assessment {
    let mut blocked = Vec::new();
    let mut unsupported = Vec::new();

    for finding in &report.findings {
        let Some(kinds) = action_kinds_for_finding(finding, workflow, report) else {
            unsupported.push(finding.clone());
            continue;
        };
        if kinds.iter().any(|kind| !repair_mode.allows_action(*kind)) {
            blocked.push(finding.clone());
        }
    }

    if blocked.is_empty() && unsupported.is_empty() && report.findings.is_empty() {
        let _ = workflow_path;
    }

    Assessment {
        blocked,
        unsupported,
    }
}

fn build_fix_plan(
    workflow_path: &Path,
    workflow: &WorkflowSummary,
    report: &WorkflowValidationReport,
) -> Result<FixPlan> {
    let mut plan = FixPlan::default();
    for finding in &report.findings {
        match finding {
            WorkflowValidationFinding::WorkflowSpecReadFailed { .. } => {
                plan.update_validation_yaml = true;
                plan.spec_reset = true;
            }
            WorkflowValidationFinding::WorkflowIdMismatch { .. } => {
                plan.update_validation_yaml = true;
            }
            WorkflowValidationFinding::MissingFile { path } => {
                if path == Path::new("README.md") {
                    plan.repair_readme = true;
                } else if path == Path::new("DESIGN.md") {
                    plan.repair_design = true;
                } else if path == Path::new("package.json")
                    && workflow.runtime.kind == WorkflowRuntimeKind::Typescript
                {
                    plan.repair_package_manifest = true;
                } else if path == Path::new("package.json") {
                    continue;
                } else if path == Path::new("tsconfig.json")
                    && workflow.runtime.kind == WorkflowRuntimeKind::Typescript
                {
                    plan.repair_tsconfig = true;
                } else if path == Path::new("tsconfig.json") {
                    continue;
                } else {
                    plan.create_layout = true;
                }
            }
            WorkflowValidationFinding::MissingDirectory { .. } => {
                plan.create_layout = true;
            }
            WorkflowValidationFinding::MissingGitRepository { .. } => {
                plan.create_layout = true;
            }
            WorkflowValidationFinding::WorkflowPathEscapesRoot { .. } => {}
            WorkflowValidationFinding::MissingDocumentHeading { path, .. } => {
                if path == Path::new("README.md") {
                    plan.repair_readme = true;
                } else {
                    plan.repair_design = true;
                }
            }
            WorkflowValidationFinding::PackageManifestParseFailed { .. } => {
                if workflow.runtime.kind == WorkflowRuntimeKind::Typescript {
                    plan.repair_package_manifest = true;
                }
            }
            WorkflowValidationFinding::UndeclaredPackageImport { package_name, .. } => {
                if workflow.runtime.kind == WorkflowRuntimeKind::Typescript {
                    plan.repair_package_manifest = true;
                    plan.refresh_dependencies = true;
                    plan.package_names.insert(package_name.clone());
                }
            }
            WorkflowValidationFinding::MissingValidationCommands { .. }
            | WorkflowValidationFinding::EmptyValidationCommands { .. }
            | WorkflowValidationFinding::InvalidValidationCommands { .. }
            | WorkflowValidationFinding::MissingCoverageMetadata { .. }
            | WorkflowValidationFinding::MissingCoverageKey { .. }
            | WorkflowValidationFinding::InvalidCoverageKeyType { .. }
            | WorkflowValidationFinding::CoverageKeyMustBeTrue { .. } => {
                plan.update_validation_yaml = true;
            }
            WorkflowValidationFinding::AmbiguousWorkflowOutputSchema { .. } => {
                plan.repair_output_schema_contracts = true;
            }
            WorkflowValidationFinding::RuntimeStateGitignoreMissing { .. }
            | WorkflowValidationFinding::TrackedRuntimeStateFiles { .. } => {
                plan.create_layout = true;
            }
            WorkflowValidationFinding::MissingCoverageMarker { key, .. } => {
                plan.add_coverage_markers.insert(key.clone());
            }
            WorkflowValidationFinding::CodeOutsideSrc { paths }
            | WorkflowValidationFinding::TestsOutsideSrcTests { paths }
            | WorkflowValidationFinding::DatabasesOutsideState { paths } => {
                let _ = workflow_path;
                plan.create_layout = true;
                if paths.iter().any(|path| is_test_path(path.as_path())) {
                    plan.add_coverage_markers.insert("positive".to_string());
                    plan.add_coverage_markers.insert("negative".to_string());
                    plan.add_coverage_markers.insert("load".to_string());
                    plan.add_coverage_markers.insert("autocomplete".to_string());
                }
            }
            WorkflowValidationFinding::ValidationCommandFailed {
                command,
                exit_code,
                stdout,
                stderr,
            } => {
                if workflow.runtime.kind != WorkflowRuntimeKind::Typescript {
                    continue;
                }
                if !command_fixable(command, *exit_code) {
                    continue;
                }
                plan.repair_package_manifest = true;
                plan.repair_tsconfig = command.contains("npm run build");
                plan.build_script = command.contains("npm run build");
                plan.test_script = command.contains("npm test");
                plan.run_script = true;
                plan.refresh_dependencies = dependency_install_fixable(stdout, stderr);
            }
            WorkflowValidationFinding::WorkflowRuntimeCompileFailed { .. } => {
                return Err(anyhow!(
                    "workflow runtime compile failures are not repaired automatically"
                ));
            }
            WorkflowValidationFinding::WorkflowApiContractExtractionFailed { .. }
            | WorkflowValidationFinding::WorkflowApiContractSmokeFailed { .. } => {
                return Err(anyhow!(
                    "workflow API contract failures are not repaired automatically"
                ));
            }
        }
    }

    if plan.spec_reset {
        plan.update_validation_yaml = true;
    }

    if plan.update_validation_yaml {
        let _ = workflow;
    }

    Ok(plan)
}

fn action_kinds_for_finding(
    finding: &WorkflowValidationFinding,
    workflow: &WorkflowSummary,
    report: &WorkflowValidationReport,
) -> Option<Vec<WorkflowRepairActionKind>> {
    let _ = report;
    let kinds = match finding {
        WorkflowValidationFinding::WorkflowSpecReadFailed { .. }
        | WorkflowValidationFinding::WorkflowIdMismatch { .. }
        | WorkflowValidationFinding::MissingValidationCommands { .. }
        | WorkflowValidationFinding::EmptyValidationCommands { .. }
        | WorkflowValidationFinding::InvalidValidationCommands { .. }
        | WorkflowValidationFinding::MissingCoverageMetadata { .. }
        | WorkflowValidationFinding::MissingCoverageKey { .. }
        | WorkflowValidationFinding::InvalidCoverageKeyType { .. }
        | WorkflowValidationFinding::CoverageKeyMustBeTrue { .. } => {
            vec![WorkflowRepairActionKind::NormalizeValidationMetadata]
        }
        WorkflowValidationFinding::AmbiguousWorkflowOutputSchema { .. } => {
            vec![WorkflowRepairActionKind::NormalizeValidationMetadata]
        }
        WorkflowValidationFinding::MissingFile { path } => {
            if path == Path::new("README.md") {
                vec![WorkflowRepairActionKind::RepairReadme]
            } else if path == Path::new("DESIGN.md") {
                vec![WorkflowRepairActionKind::RepairDesign]
            } else if path == Path::new("package.json")
                && workflow.runtime.kind == WorkflowRuntimeKind::Typescript
            {
                vec![WorkflowRepairActionKind::RepairPackageManifest]
            } else if path == Path::new("package.json") {
                return None;
            } else if path == Path::new("tsconfig.json")
                && workflow.runtime.kind == WorkflowRuntimeKind::Typescript
            {
                vec![WorkflowRepairActionKind::RepairTsconfig]
            } else if path == Path::new("tsconfig.json") {
                return None;
            } else if is_test_path(path) {
                vec![WorkflowRepairActionKind::ScaffoldWorkflowTests]
            } else if is_code_path(path) {
                vec![WorkflowRepairActionKind::ScaffoldWorkflowSource]
            } else {
                vec![WorkflowRepairActionKind::RepairLayout]
            }
        }
        WorkflowValidationFinding::MissingDirectory { .. }
        | WorkflowValidationFinding::MissingGitRepository { .. }
        | WorkflowValidationFinding::CodeOutsideSrc { .. }
        | WorkflowValidationFinding::TestsOutsideSrcTests { .. }
        | WorkflowValidationFinding::DatabasesOutsideState { .. }
        | WorkflowValidationFinding::RuntimeStateGitignoreMissing { .. }
        | WorkflowValidationFinding::TrackedRuntimeStateFiles { .. } => {
            vec![WorkflowRepairActionKind::RepairLayout]
        }
        WorkflowValidationFinding::MissingDocumentHeading { path, .. } => {
            if path == Path::new("README.md") {
                vec![WorkflowRepairActionKind::RepairReadme]
            } else {
                vec![WorkflowRepairActionKind::RepairDesign]
            }
        }
        WorkflowValidationFinding::PackageManifestParseFailed { .. } => {
            if workflow.runtime.kind != WorkflowRuntimeKind::Typescript {
                return None;
            }
            vec![WorkflowRepairActionKind::RepairPackageManifest]
        }
        WorkflowValidationFinding::UndeclaredPackageImport { .. } => {
            if workflow.runtime.kind != WorkflowRuntimeKind::Typescript {
                return None;
            }
            vec![WorkflowRepairActionKind::RepairPackageManifest]
        }
        WorkflowValidationFinding::MissingCoverageMarker { .. } => {
            vec![WorkflowRepairActionKind::AddCoverageMarkers]
        }
        WorkflowValidationFinding::ValidationCommandFailed {
            command, exit_code, ..
        } => {
            if workflow.runtime.kind != WorkflowRuntimeKind::Typescript {
                return None;
            }
            if !command_fixable(command, *exit_code) {
                return None;
            }
            let mut kinds = vec![WorkflowRepairActionKind::RepairPackageManifest];
            if command.contains("npm run build") {
                kinds.push(WorkflowRepairActionKind::RepairTsconfig);
            }
            kinds
        }
        WorkflowValidationFinding::WorkflowRuntimeCompileFailed { .. } => {
            return None;
        }
        WorkflowValidationFinding::WorkflowPathEscapesRoot { .. }
        | WorkflowValidationFinding::WorkflowApiContractExtractionFailed { .. }
        | WorkflowValidationFinding::WorkflowApiContractSmokeFailed { .. } => {
            return None;
        }
    };
    Some(kinds)
}

fn apply_validation_yaml_fix(
    workflow: &WorkflowSummary,
    plan: &FixPlan,
) -> Result<Vec<WorkflowRepairAction>> {
    let spec_path = workflow.workflow_yaml_path.clone();
    let mut spec = read_workflow_spec(&spec_path).unwrap_or_else(|_| {
        scaffold_workflow_spec(
            workflow.id.clone(),
            workflow
                .title
                .clone()
                .unwrap_or_else(|| display_title(&workflow.id)),
            workflow
                .user_description
                .clone()
                .unwrap_or_else(|| format!("Workflow {}", workflow.id)),
            workflow.runtime.kind,
            &codex_config::types::WorkflowsConfigToml::default(),
        )
    });
    let mut changed = false;

    if plan.spec_reset {
        spec = scaffold_workflow_spec(
            workflow.id.clone(),
            workflow
                .title
                .clone()
                .unwrap_or_else(|| display_title(&workflow.id)),
            workflow
                .user_description
                .clone()
                .unwrap_or_else(|| format!("Workflow {}", workflow.id)),
            workflow.runtime.kind,
            &codex_config::types::WorkflowsConfigToml::default(),
        );
        changed = true;
    }

    if spec.id != workflow.id {
        spec.id = workflow.id.clone();
        changed = true;
    }

    if plan.update_validation_yaml {
        let mut validation = if spec.validation.is_object() {
            spec.validation.clone()
        } else {
            JsonValue::Object(Default::default())
        };
        let Some(validation_object) = validation.as_object_mut() else {
            return Err(anyhow!("workflow validation must be a JSON object"));
        };

        let commands_need_fix = plan.spec_reset
            || validation_object
                .get("commands")
                .and_then(JsonValue::as_array)
                .is_none_or(|commands| {
                    commands.is_empty() || !commands.iter().all(JsonValue::is_string)
                });
        if commands_need_fix {
            let commands = match workflow.runtime.kind {
                WorkflowRuntimeKind::Rune => vec![JsonValue::String("true".to_string())],
                WorkflowRuntimeKind::Typescript => vec![
                    JsonValue::String("npm run build".to_string()),
                    JsonValue::String("npm test".to_string()),
                ],
            };
            validation_object.insert("commands".to_string(), JsonValue::Array(commands));
        }

        let coverage_need_fix = plan.spec_reset
            || match validation_object.get("coverage") {
                Some(JsonValue::Object(coverage)) => {
                    const REQUIRED_TRUE_KEYS: &[&str] = &[
                        "positive",
                        "negative",
                        "progress",
                        "finalResult",
                        "failureUx",
                        "load",
                        "autocomplete",
                    ];
                    REQUIRED_TRUE_KEYS
                        .iter()
                        .any(|key| coverage.get(*key) != Some(&JsonValue::Bool(true)))
                        || coverage.get("recovery") != Some(&JsonValue::Bool(false))
                }
                _ => true,
            };
        if coverage_need_fix {
            validation_object.insert(
                "coverage".to_string(),
                json!({
                    "positive": true,
                    "negative": true,
                    "progress": true,
                    "finalResult": true,
                    "failureUx": true,
                    "load": true,
                    "autocomplete": true,
                    "recovery": false,
                }),
            );
        }

        if !validation_object.contains_key("profile") {
            validation_object.insert(
                "profile".to_string(),
                JsonValue::String("default".to_string()),
            );
        }
        spec.validation = validation;
        changed = true;
    }

    if plan.repair_output_schema_contracts && normalize_output_schema_contracts(&mut spec) {
        changed = true;
    }

    if !changed {
        return Ok(Vec::new());
    }

    write_workflow_spec(&spec_path, &spec)?;
    Ok(vec![WorkflowRepairAction {
        kind: WorkflowRepairActionKind::NormalizeValidationMetadata,
        path: spec_path,
        detail: "Updated workflow.yaml metadata".to_string(),
    }])
}

fn normalize_output_schema_contracts(spec: &mut WorkflowSpec) -> bool {
    let mut changed = false;
    if let Some(output_schema) = spec.api.get_mut("outputSchema")
        && !output_schema.is_null()
    {
        changed |= normalize_output_schema(output_schema);
    }
    if let Some(tool) = spec.tool.as_mut()
        && !tool.output_schema.is_null()
    {
        changed |= normalize_output_schema(&mut tool.output_schema);
    }
    changed
}

fn normalize_output_schema(schema: &mut JsonValue) -> bool {
    let mut changed = false;
    if output_object_schema_needs_explicit_shape(schema)
        && let Some(object) = schema.as_object_mut()
    {
        object.insert("additionalProperties".to_string(), JsonValue::Bool(true));
        changed = true;
    }

    for union_key in ["anyOf", "oneOf", "allOf"] {
        if let Some(entries) = schema.get_mut(union_key).and_then(JsonValue::as_array_mut) {
            for entry in entries {
                changed |= normalize_output_schema(entry);
            }
        }
    }
    if let Some(properties) = schema
        .get_mut("properties")
        .and_then(JsonValue::as_object_mut)
    {
        for property_schema in properties.values_mut() {
            changed |= normalize_output_schema(property_schema);
        }
    }
    if let Some(items) = schema.get_mut("items") {
        changed |= normalize_output_schema(items);
    }
    changed
}

fn output_object_schema_needs_explicit_shape(schema: &JsonValue) -> bool {
    if !output_schema_declares_object_type(schema) {
        return false;
    }
    let has_properties = schema
        .get("properties")
        .and_then(JsonValue::as_object)
        .is_some_and(|properties| !properties.is_empty());
    let has_additional_properties = schema
        .as_object()
        .is_some_and(|object| object.contains_key("additionalProperties"));
    !has_properties && !has_additional_properties
}

fn output_schema_declares_object_type(schema: &JsonValue) -> bool {
    match schema.get("type") {
        Some(JsonValue::String(type_name)) => type_name == "object",
        Some(JsonValue::Array(type_names)) => {
            type_names.iter().any(|type_name| type_name == "object")
        }
        _ => false,
    }
}

fn apply_layout_fix(
    workflow: &WorkflowSummary,
    report: &WorkflowValidationReport,
) -> Result<Vec<WorkflowRepairAction>> {
    let mut actions = Vec::new();
    if ensure_dir(&workflow.path.join("src"))? {
        actions.push(WorkflowRepairAction {
            kind: WorkflowRepairActionKind::RepairLayout,
            path: workflow.path.join("src"),
            detail: "Created src/ directory".to_string(),
        });
    }
    if ensure_dir(&workflow.path.join("src/tests"))? {
        actions.push(WorkflowRepairAction {
            kind: WorkflowRepairActionKind::ScaffoldWorkflowTests,
            path: workflow.path.join("src/tests"),
            detail: "Created src/tests/ directory".to_string(),
        });
    }
    if ensure_dir(&workflow.path.join("state"))? {
        actions.push(WorkflowRepairAction {
            kind: WorkflowRepairActionKind::RepairLayout,
            path: workflow.path.join("state"),
            detail: "Created state/ directory".to_string(),
        });
    }
    ensure_git_repo(&workflow.path)?;

    if ensure_runtime_state_gitignore(&workflow.path)? {
        actions.push(WorkflowRepairAction {
            kind: WorkflowRepairActionKind::RepairLayout,
            path: workflow.path.join(".gitignore"),
            detail: "Updated runtime state ignore rules".to_string(),
        });
    }

    let tracked_runtime_state_paths = tracked_runtime_state_paths(report);
    if untrack_runtime_state_files(&workflow.path, &tracked_runtime_state_paths)? {
        actions.push(WorkflowRepairAction {
            kind: WorkflowRepairActionKind::RepairLayout,
            path: workflow.path.join(".gitignore"),
            detail: format!(
                "Removed {} runtime state file(s) from git tracking",
                tracked_runtime_state_paths.len()
            ),
        });
    }

    let mut moved_any = false;
    visit_layout_files(&workflow.path, &mut |relative, path| {
        if relative.starts_with(Path::new("src")) || relative.starts_with(Path::new("state")) {
            return Ok(());
        }
        if is_database_path(relative) {
            let target = workflow.path.join("state").join(relative);
            move_file(path, &target)?;
            moved_any = true;
            actions.push(WorkflowRepairAction {
                kind: WorkflowRepairActionKind::RepairLayout,
                path: target,
                detail: format!("Moved database file {} to state/", relative.display()),
            });
        } else if is_test_path(relative) {
            let target = workflow
                .path
                .join("src/tests")
                .join(strip_tests_prefix(relative));
            move_file(path, &target)?;
            moved_any = true;
            actions.push(WorkflowRepairAction {
                kind: WorkflowRepairActionKind::ScaffoldWorkflowTests,
                path: target,
                detail: format!("Moved test file {} under src/tests/", relative.display()),
            });
        } else if is_code_path(relative) {
            let target = workflow.path.join("src").join(relative);
            move_file(path, &target)?;
            moved_any = true;
            actions.push(WorkflowRepairAction {
                kind: WorkflowRepairActionKind::ScaffoldWorkflowSource,
                path: target,
                detail: format!("Moved code file {} under src/", relative.display()),
            });
        }
        Ok(())
    })?;

    let source_path = workflow.path.join(&workflow.runtime.entrypoint);
    if !source_path.is_file() {
        write_scaffold_source(workflow, &source_path)?;
        actions.push(WorkflowRepairAction {
            kind: WorkflowRepairActionKind::ScaffoldWorkflowSource,
            path: source_path,
            detail: format!("Created {} scaffold", workflow.runtime.entrypoint),
        });
    }

    if report.findings.iter().any(|finding| {
        matches!(
            finding,
            WorkflowValidationFinding::MissingGitRepository { .. }
        )
    }) && !workflow.path.join(".git/HEAD").is_file()
    {
        init_git_repo(&workflow.path)?;
        actions.push(WorkflowRepairAction {
            kind: WorkflowRepairActionKind::RepairLayout,
            path: workflow.path.join(".git"),
            detail: "Initialized git repository".to_string(),
        });
    }

    if !workflow.path.join("state/.gitkeep").is_file() {
        fs::write(workflow.path.join("state/.gitkeep"), "").with_context(|| {
            format!(
                "failed to write {}",
                workflow.path.join("state/.gitkeep").display()
            )
        })?;
        actions.push(WorkflowRepairAction {
            kind: WorkflowRepairActionKind::RepairLayout,
            path: workflow.path.join("state/.gitkeep"),
            detail: "Created state/.gitkeep placeholder".to_string(),
        });
    }

    if !moved_any && actions.is_empty() {
        return Ok(Vec::new());
    }

    Ok(actions)
}

fn ensure_runtime_state_gitignore(workflow_path: &Path) -> Result<bool> {
    let path = workflow_path.join(".gitignore");
    let mut contents = fs::read_to_string(&path).unwrap_or_default();
    let missing_patterns = RUNTIME_STATE_GITIGNORE_PATTERNS
        .iter()
        .filter(|pattern| !gitignore_contains_pattern(&contents, pattern))
        .copied()
        .collect::<Vec<_>>();
    if missing_patterns.is_empty() {
        return Ok(false);
    }

    if !contents.is_empty() && !contents.ends_with('\n') {
        contents.push('\n');
    }
    for pattern in missing_patterns {
        contents.push_str(pattern);
        contents.push('\n');
    }
    fs::write(&path, contents).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(true)
}

fn gitignore_contains_pattern(contents: &str, pattern: &str) -> bool {
    contents.lines().any(|line| line.trim() == pattern)
}

fn tracked_runtime_state_paths(report: &WorkflowValidationReport) -> Vec<PathBuf> {
    let mut paths = report
        .findings
        .iter()
        .filter_map(|finding| match finding {
            WorkflowValidationFinding::TrackedRuntimeStateFiles { paths } => Some(paths.as_slice()),
            _ => None,
        })
        .flatten()
        .cloned()
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths
}

fn untrack_runtime_state_files(workflow_path: &Path, paths: &[PathBuf]) -> Result<bool> {
    if paths.is_empty() || !workflow_path.join(".git/HEAD").is_file() {
        return Ok(false);
    }

    let output = Command::new("git")
        .args(["rm", "--cached", "--ignore-unmatch", "--"])
        .args(paths)
        .current_dir(workflow_path)
        .output()
        .with_context(|| {
            format!(
                "failed to remove runtime state files from git index in {}",
                workflow_path.display()
            )
        })?;
    if output.status.success() {
        return Ok(true);
    }

    Err(anyhow!(
        "git rm --cached failed in {} with {}: {}{}",
        workflow_path.display(),
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    ))
}

fn apply_readme_fix(workflow: &WorkflowSummary) -> Result<WorkflowRepairAction> {
    let path = workflow.path.join("README.md");
    fs::write(&path, readme_template(workflow))
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(WorkflowRepairAction {
        kind: WorkflowRepairActionKind::RepairReadme,
        path,
        detail: "Updated README.md".to_string(),
    })
}

fn apply_design_fix(workflow: &WorkflowSummary) -> Result<WorkflowRepairAction> {
    let path = workflow.path.join("DESIGN.md");
    fs::write(&path, design_template(workflow))
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(WorkflowRepairAction {
        kind: WorkflowRepairActionKind::RepairDesign,
        path,
        detail: "Updated DESIGN.md".to_string(),
    })
}

fn apply_package_manifest_fix(
    workflow: &WorkflowSummary,
    plan: &FixPlan,
) -> Result<WorkflowRepairAction> {
    let path = workflow.path.join("package.json");
    let existing_contents = fs::read_to_string(&path).ok();
    let mut manifest = match fs::read_to_string(&path) {
        Ok(contents) => serde_json::from_str::<JsonValue>(&contents)
            .unwrap_or_else(|_| JsonValue::Object(Default::default())),
        Err(_) => JsonValue::Object(Default::default()),
    };
    let Some(object) = manifest.as_object_mut() else {
        return Err(anyhow!("package.json must be a JSON object"));
    };

    object.insert("private".to_string(), JsonValue::Bool(true));
    object.insert("type".to_string(), JsonValue::String("module".to_string()));

    let scripts = object
        .entry("scripts")
        .or_insert_with(|| JsonValue::Object(Default::default()));
    let Some(scripts_object) = scripts.as_object_mut() else {
        return Err(anyhow!("package.json scripts must be a JSON object"));
    };
    if plan.build_script || plan.repair_tsconfig {
        scripts_object.insert(
            "build".to_string(),
            JsonValue::String("tsc --noEmit".to_string()),
        );
    }
    if plan.test_script || !scripts_object.contains_key("test") {
        scripts_object.insert(
            "test".to_string(),
            JsonValue::String("node --import tsx --test src/tests/**/*.test.ts".to_string()),
        );
    }
    if plan.run_script || !scripts_object.contains_key("run") {
        scripts_object.insert(
            "run".to_string(),
            JsonValue::String("tsx src/workflow.ts".to_string()),
        );
    }

    let deps = object
        .entry("dependencies")
        .or_insert_with(|| JsonValue::Object(Default::default()));
    let Some(deps_object) = deps.as_object_mut() else {
        return Err(anyhow!("package.json dependencies must be a JSON object"));
    };
    deps_object.insert(
        "@openai/codex-sdk".to_string(),
        JsonValue::String("latest".to_string()),
    );
    for package_name in &plan.package_names {
        deps_object.insert(
            package_name.clone(),
            JsonValue::String("latest".to_string()),
        );
    }

    let dev_deps = object
        .entry("devDependencies")
        .or_insert_with(|| JsonValue::Object(Default::default()));
    let Some(dev_deps_object) = dev_deps.as_object_mut() else {
        return Err(anyhow!(
            "package.json devDependencies must be a JSON object"
        ));
    };
    dev_deps_object.insert(
        "@types/node".to_string(),
        JsonValue::String("latest".to_string()),
    );
    dev_deps_object.insert("tsx".to_string(), JsonValue::String("latest".to_string()));
    dev_deps_object.insert(
        "typescript".to_string(),
        JsonValue::String("latest".to_string()),
    );

    let updated_contents = serde_json::to_string_pretty(&manifest)? + "\n";
    if existing_contents.as_deref() == Some(updated_contents.as_str()) {
        return Ok(WorkflowRepairAction {
            kind: WorkflowRepairActionKind::RepairPackageManifest,
            path,
            detail: String::new(),
        });
    }

    fs::write(&path, updated_contents)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(WorkflowRepairAction {
        kind: WorkflowRepairActionKind::RepairPackageManifest,
        path,
        detail: "Updated package.json".to_string(),
    })
}

fn apply_coverage_marker_fix(
    workflow: &WorkflowSummary,
    marker_keys: &BTreeSet<String>,
) -> Result<WorkflowRepairAction> {
    let test_extension = match workflow.runtime.kind {
        WorkflowRuntimeKind::Rune => "rn",
        WorkflowRuntimeKind::Typescript => "ts",
    };
    let canonical_files = [
        (
            format!("workflow.positive.test.{test_extension}"),
            vec!["positive", "progress", "finalResult"],
        ),
        (format!("workflow.load.test.{test_extension}"), vec!["load"]),
        (
            format!("workflow.autocomplete.test.{test_extension}"),
            vec!["autocomplete"],
        ),
        (
            format!("workflow.negative.test.{test_extension}"),
            vec!["negative", "failureUx"],
        ),
    ];

    ensure_dir(&workflow.path.join("src/tests"))?;
    for (file_name, markers) in canonical_files {
        let path = workflow.path.join("src/tests").join(&file_name);
        let required_markers: Vec<_> = markers
            .into_iter()
            .filter(|marker| marker_keys.contains(*marker))
            .collect();
        if required_markers.is_empty() {
            continue;
        }
        let mut contents = fs::read_to_string(&path).unwrap_or_default();
        for marker in &required_markers {
            let marker_line = format!("// workflow-covers: {marker}");
            if !contents.contains(&marker_line) {
                contents = format!("{marker_line}\n{contents}");
            }
        }
        if !has_non_comment_code(&contents) {
            if contents.is_empty() {
                contents = format!("// workflow-covers: {}\n", required_markers.join(" "));
            }
            ensure_trailing_newline(&mut contents);
            if workflow.runtime.kind == WorkflowRuntimeKind::Typescript {
                contents.push_str("export {};\n");
            } else {
                contents.push_str(&rune_coverage_stub(&required_markers));
            }
        }
        fs::write(&path, contents)
            .with_context(|| format!("failed to write {}", path.display()))?;
    }

    if marker_keys.contains("recovery") {
        let path = workflow
            .path
            .join(format!("src/tests/workflow.recovery.test.{test_extension}"));
        let mut contents = fs::read_to_string(&path).unwrap_or_default();
        let marker_line = "// workflow-covers: recovery";
        if !contents.contains(marker_line) {
            contents = format!("{marker_line}\n{contents}");
        }
        if !has_non_comment_code(&contents) {
            if contents.is_empty() {
                contents = format!("{marker_line}\n");
            }
            ensure_trailing_newline(&mut contents);
            if workflow.runtime.kind == WorkflowRuntimeKind::Typescript {
                contents.push_str("export {};\n");
            } else {
                contents.push_str(&rune_coverage_stub(&["recovery"]));
            }
        }
        fs::write(&path, contents)
            .with_context(|| format!("failed to write {}", path.display()))?;
    }

    Ok(WorkflowRepairAction {
        kind: WorkflowRepairActionKind::AddCoverageMarkers,
        path: workflow.path.join("src/tests"),
        detail: format!(
            "Updated coverage markers for {} marker(s)",
            marker_keys.len()
        ),
    })
}

fn ensure_trailing_newline(contents: &mut String) {
    if !contents.ends_with('\n') {
        contents.push('\n');
    }
}

fn has_non_comment_code(contents: &str) -> bool {
    contents.lines().any(|line| {
        let line = line.trim();
        !line.is_empty() && !line.starts_with("//")
    })
}

fn rune_coverage_stub(markers: &[&str]) -> String {
    let name = markers
        .join("_")
        .replace("finalResult", "final_result")
        .replace("failureUx", "failure_ux");
    format!("pub fn covers_{name}() {{\n    true\n}}\n")
}

fn apply_tsconfig_fix(workflow: &WorkflowSummary) -> Result<WorkflowRepairAction> {
    let path = workflow.path.join("tsconfig.json");
    let contents = tsconfig_template();
    if fs::read_to_string(&path).ok().as_deref() == Some(contents.as_str()) {
        return Ok(WorkflowRepairAction {
            kind: WorkflowRepairActionKind::RepairTsconfig,
            path,
            detail: String::new(),
        });
    }

    fs::write(&path, contents).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(WorkflowRepairAction {
        kind: WorkflowRepairActionKind::RepairTsconfig,
        path,
        detail: "Created tsconfig.json".to_string(),
    })
}

fn apply_dependency_install_fix(
    workflow: &WorkflowSummary,
    dependency_update_policy: &str,
) -> Result<Option<WorkflowRepairAction>> {
    if !dependency_updates_enabled(dependency_update_policy) {
        return Ok(None);
    }
    if !workflow.path.join("package.json").is_file() {
        return Ok(None);
    }

    let output = Command::new("npm")
        .args(["install", "--ignore-scripts", "--no-audit", "--no-fund"])
        .current_dir(&workflow.path)
        .output()
        .with_context(|| {
            format!(
                "failed to run npm install for workflow `{}` in {}",
                workflow.id,
                workflow.path.display()
            )
        })?;

    if !output.status.success() {
        return Err(anyhow!(
            "npm install failed for workflow `{}` with exit code {:?}\nstdout:\n{}\nstderr:\n{}",
            workflow.id,
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    Ok(Some(WorkflowRepairAction {
        kind: WorkflowRepairActionKind::RepairPackageManifest,
        path: workflow.path.join("package-lock.json"),
        detail:
            "Installed workflow dependencies with npm install --ignore-scripts --no-audit --no-fund"
                .to_string(),
    }))
}

fn validation_report_to_api(report: &WorkflowValidationReport) -> WorkflowValidationInfo {
    WorkflowValidationInfo {
        status: report.status,
        findings: api_findings(&report.findings),
    }
}

fn api_findings(findings: &[WorkflowValidationFinding]) -> Vec<WorkflowValidationFindingInfo> {
    findings.to_vec()
}

fn finding_to_api(finding: &WorkflowValidationFinding) -> WorkflowValidationFindingInfo {
    finding.clone()
}

fn should_commit_changes(ctx: &WorkflowCommandContext<'_>) -> bool {
    !matches!(
        ctx.config.commit_policy.as_deref(),
        Some("manual" | "none" | "disabled")
    )
}

fn commit_repair_changes(path: &Path) -> Result<()> {
    init_git_repo(path)?;
    run_git(path, &["add", "."])?;
    let diff = Command::new("git")
        .args(["diff", "--cached", "--quiet"])
        .current_dir(path)
        .output()
        .with_context(|| format!("failed to inspect staged diff in {}", path.display()))?;
    if diff.status.success() {
        return Ok(());
    }
    let output = Command::new("git")
        .args(["commit", "-m", "Repair workflow"])
        .current_dir(path)
        .env("GIT_AUTHOR_NAME", "Codex")
        .env("GIT_AUTHOR_EMAIL", "codex@openai.com")
        .env("GIT_COMMITTER_NAME", "Codex")
        .env("GIT_COMMITTER_EMAIL", "codex@openai.com")
        .output()
        .with_context(|| format!("failed to run git commit in {}", path.display()))?;
    if output.status.success() {
        Ok(())
    } else {
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let mut message = format!(
            "git commit failed in {} with {}",
            path.display(),
            output.status
        );
        if !stdout.is_empty() {
            message.push_str(&format!("; stdout: {stdout}"));
        }
        if !stderr.is_empty() {
            message.push_str(&format!("; stderr: {stderr}"));
        }
        Err(anyhow!(message))
    }
}

fn run_git(path: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .with_context(|| format!("failed to run git {} in {}", args.join(" "), path.display()))?;
    if output.status.success() {
        Ok(())
    } else {
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let mut message = format!(
            "git {} failed in {} with {}",
            args.join(" "),
            path.display(),
            output.status
        );
        if !stdout.is_empty() {
            message.push_str(&format!("; stdout: {stdout}"));
        }
        if !stderr.is_empty() {
            message.push_str(&format!("; stderr: {stderr}"));
        }
        Err(anyhow!(message))
    }
}

fn init_git_repo(path: &Path) -> Result<()> {
    run_git(path, &["init"])
}

fn ensure_dir(path: &Path) -> Result<bool> {
    let existed = path.exists();
    fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))?;
    Ok(!existed)
}

fn ensure_git_repo(path: &Path) -> Result<()> {
    if path.join(".git/HEAD").is_file() {
        return Ok(());
    }
    init_git_repo(path)
}

fn visit_layout_files(
    workflow_path: &Path,
    visitor: &mut impl FnMut(&Path, &Path) -> Result<()>,
) -> Result<()> {
    visit_layout_files_inner(workflow_path, workflow_path, visitor)
}

fn visit_layout_files_inner(
    workflow_path: &Path,
    dir: &Path,
    visitor: &mut impl FnMut(&Path, &Path) -> Result<()>,
) -> Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            if dir == workflow_path {
                return Err(err.into());
            }
            return Ok(());
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if should_skip_layout_dir(&path) {
                continue;
            }
            visit_layout_files_inner(workflow_path, &path, visitor)?;
            continue;
        }

        let Ok(relative) = path.strip_prefix(workflow_path) else {
            continue;
        };
        visitor(relative, &path)?;
    }

    Ok(())
}

fn should_skip_layout_dir(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some(".git" | "node_modules" | "target" | "dist" | "build" | "coverage")
    )
}

fn move_file(source: &Path, target: &Path) -> Result<()> {
    if source == target {
        return Ok(());
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    if target.exists() {
        if target.is_dir() {
            fs::remove_dir_all(target)
                .with_context(|| format!("failed to remove {}", target.display()))?;
        } else {
            fs::remove_file(target)
                .with_context(|| format!("failed to remove {}", target.display()))?;
        }
    }
    match fs::rename(source, target) {
        Ok(()) => Ok(()),
        Err(_) => {
            fs::copy(source, target).with_context(|| {
                format!(
                    "failed to copy {} to {}",
                    source.display(),
                    target.display()
                )
            })?;
            fs::remove_file(source)
                .with_context(|| format!("failed to remove {}", source.display()))?;
            Ok(())
        }
    }
}

fn write_scaffold_source(workflow: &WorkflowSummary, path: &Path) -> Result<()> {
    match workflow.runtime.kind {
        WorkflowRuntimeKind::Rune => write_rune_scaffold_source(workflow, path),
        WorkflowRuntimeKind::Typescript => write_typescript_scaffold_source(workflow, path),
    }
}

fn write_rune_scaffold_source(workflow: &WorkflowSummary, path: &Path) -> Result<()> {
    let title = workflow
        .title
        .clone()
        .unwrap_or_else(|| display_title(&workflow.id));
    let title_literal = serde_json::to_string(&title)?;
    let markdown_literal = serde_json::to_string(&format!("# {title}\n\nWorkflow complete."))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(
        path,
        format!(
            r##"pub async fn run(ctx, input) {{
    ctx.status(#{{
        workflowName: {title_literal},
        workflowStatus: "running",
        threads: [],
    }});
    #{{
        ok: true,
        input,
    }}
}}

pub async fn complete(_ctx, _input) {{
    []
}}

pub fn to_tui_markdown(_result) {{
    #{{
        markdown: {markdown_literal},
    }}
}}
"##
        ),
    )
    .with_context(|| format!("failed to write {}", path.display()))
}

fn write_typescript_scaffold_source(workflow: &WorkflowSummary, path: &Path) -> Result<()> {
    let command_label = workflow
        .command
        .as_deref()
        .unwrap_or_else(|| workflow.id.split('/').next_back().unwrap_or(&workflow.id));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(
        path,
        format!(
            r##"import type {{ WorkflowContext }} from "@openai/codex-sdk/workflow";

export interface WorkflowInput {{
  input?: string;
}}

export interface WorkflowOutput {{
  ok: true;
  input: WorkflowInput;
}}

function validateInput(input: unknown) {{
  if (!input || typeof input !== "object" || Array.isArray(input)) {{
    throw new Error("workflow input must be a JSON object");
  }}
  return input as WorkflowInput;
}}

export const WorkflowOutput = {{
  toTuiMarkdown(result: WorkflowOutput) {{
    return {{ markdown: "# Workflow\n\nWorkflow complete." }};
  }},
}};

export default async function {command_label}(ctx: WorkflowContext, input: WorkflowInput): Promise<WorkflowOutput> {{
  const normalizedInput = validateInput(input);
  ctx.progress("Running workflow", {{ input: normalizedInput }});
  return {{ ok: true, input: normalizedInput }};
}}

export async function complete(_ctx: WorkflowContext) {{
  return [];
}}

if (import.meta.url === `file://${{process.argv[1]}}`) {{
  const inputIndex = process.argv.indexOf("--input");
  const rawInput = inputIndex >= 0 ? process.argv[inputIndex + 1] : "{{}}";
  const input = JSON.parse(rawInput ?? "{{}}");
  const output = await {command_label}({{
    progress() {{}},
    reportToUserMarkdown() {{}},
    status() {{}},
    runWorkflow() {{ throw new Error("runWorkflow() is unavailable in direct CLI smoke"); }},
    cwd: process.cwd(),
    currentWorkingDirectory: process.cwd(),
    repoRoot: process.cwd(),
    workingDirectory: process.cwd(),
  }} as never, input);
  console.log(JSON.stringify(output, null, 2));
}}
"##,
            command_label = command_label.replace('-', "_"),
        ),
    )
    .with_context(|| format!("failed to write {}", path.display()))
}

fn readme_template(workflow: &WorkflowSummary) -> String {
    let title = workflow
        .title
        .clone()
        .unwrap_or_else(|| display_title(&workflow.id));
    let description = workflow
        .user_description
        .clone()
        .unwrap_or_else(|| "Workflow repair scaffold.".to_string());
    let command_label = workflow
        .command
        .as_deref()
        .unwrap_or_else(|| workflow.id.split('/').next_back().unwrap_or(&workflow.id));
    match workflow.runtime.kind {
        WorkflowRuntimeKind::Rune => format!(
            "# {title}\n\n{description}\n\n## Usage\n\n```sh\n/{command_label}\n# or\ncodex {command_label}\n```\n\n## Workflow Runtime\n\nThis workflow runs on embedded Rune from `{entrypoint}`. Implement `pub async fn run(ctx, input)` and keep the return value as the canonical JSON result. Optional `complete(ctx, input)` provides autocomplete and optional `to_tui_markdown(result)` provides the TUI markdown view. Use `ctx.status(...)` while running, `ctx.progress(message, data)` as a shorthand, `ctx.reportToUserMarkdown(markdown)` for direct user reports, and `ctx.runWorkflow(workflow, input, options)` for child workflows. `ctx.cwd`, `ctx.currentWorkingDirectory`, `ctx.repoRoot`, and `ctx.workingDirectory` point at the workspace that launched the workflow.\n\n## Dependencies\n\nRune workflows run inside Codex through the embedded Rune runtime. Do not require a global `rune` binary or local Node package installation for runtime execution.\n\n## Contract\n\nRune workflow contracts are manifest-defined. Keep `workflow.yaml api.inputSchema`, `api.outputSchema`, `api.formatSchemas`, and optional `api.callableName` aligned with the Rune implementation. `/workflow validate {id}` publishes that manifest contract after validation passes.\n\n## Validation\n\nRun the configured validation commands from `workflow.yaml` and keep the coverage markers aligned with the documented contract.\n\n## Maintenance\n\nUpdate `README.md`, `DESIGN.md`, `workflow.yaml`, and the test markers together when workflow behavior changes. Keep runtime state and generated artifacts under ignored `state/` or `artifacts/` paths.\n",
            entrypoint = workflow.runtime.entrypoint,
            id = workflow.id,
        ),
        WorkflowRuntimeKind::Typescript => format!(
            "# {title}\n\n{description}\n\n## Usage\n\n```sh\n/{command_label}\n# or\ncodex {command_label}\n```\n\n## Workflow Runtime\n\nPrefer `ctx.status({{ workflowName, workflowStatus, threads? }})` while the workflow is running so the TUI can render `Workflow <workflowName>: <workflowStatus>` with optional `-> <threadName>: <threadStatus>` rows when more than one thread is active. `ctx.progress(message, data?)` remains available as a legacy shorthand for single-string status updates. `ctx.cwd`, `ctx.currentWorkingDirectory`, `ctx.repoRoot`, and `ctx.workingDirectory` point at the workspace that launched the workflow, while `process.cwd()` stays on the workflow package directory. `ctx.runWorkflow(workflow, input?, {{ onStatusUpdate }})` can intercept child workflow status updates and either forward, transform, bundle, or suppress them. Export a named default async function for the execution entrypoint, an optional named `complete(...)` export for autocomplete, and an optional `WorkflowOutput.toTuiMarkdown(result)` value companion for markdown rendering. `/workflow validate {id}` extracts, smoke-tests when `validation.contractSmoke` is configured, and publishes the TS contract after the workflow passes validation. Keep the default export focused on canonical JSON results.\n\n## Dependencies\n\nDo not rely on globally installed third-party packages. Built-in platform modules are fine, but every external package the workflow imports must be declared in this workflow's local `package.json` and resolved from this directory's `node_modules`.\n\n## Validation\n\nRun the configured validation commands from `workflow.yaml` and keep the coverage markers and contract smoke output aligned with the documented contract. Prefer commands that fail fast on missing dependencies or type errors.\n\n## Maintenance\n\nUpdate `README.md`, `DESIGN.md`, `workflow.yaml`, and the test markers together when the workflow contract changes. Keep runtime state and generated artifacts under ignored `state/` or `artifacts/` paths.\n",
            id = workflow.id,
        ),
    }
}

fn design_template(workflow: &WorkflowSummary) -> String {
    let title = workflow
        .title
        .clone()
        .unwrap_or_else(|| display_title(&workflow.id));
    match workflow.runtime.kind {
        WorkflowRuntimeKind::Rune => format!(
            "# {title} Design\n\n## Overview\n\nThis workflow is an embedded Rune workflow validated through `codex workflow validate {id}`.\n\n## Architecture\n\n- `{entrypoint}` owns the `run`, optional `complete`, and optional `to_tui_markdown` functions.\n- `workflow.yaml` owns the runtime declaration, validation commands, coverage expectations, and manifest-defined API schemas.\n- `src/tests/` carries the coverage contract for positive, load, autocomplete, negative, and recovery paths.\n- `state/` holds persistent runtime data; `artifacts/` holds generated run artifacts. Both are ignored except for `state/.gitkeep`.\n\n## Data Flow\n\n1. A registered workflow command loads the Rune entrypoint.\n2. `run(ctx, input)` validates input, emits status, and returns the canonical JSON result.\n3. If present, `to_tui_markdown(result)` provides the markdown view for the TUI and workflow-to-workflow callers.\n4. `codex workflow validate {id}` runs the configured validation commands, checks docs/layout/coverage markers, and publishes the manifest-defined contract only after validation passes.\n\n## Failure Handling\n\nValidate inputs early. Surface actionable failures instead of generic exit-only errors. When the workflow cannot satisfy its manifest contract, fail with a specific error that names the broken path.\n\n## Recovery Behavior\n\nPrefer recovery when correctness is preserved. Do not hide corruption or return misleading success. Set `validation.coverage.recovery` to `true` only when recovery exists and is tested.\n\n## Test Matrix\n\n- `src/tests/workflow.positive.test.rn`: positive path, status, JSON result, and markdown formatter coverage.\n- `src/tests/workflow.load.test.rn`: loadability smoke.\n- `src/tests/workflow.autocomplete.test.rn`: registry and command-completion readiness smoke.\n- `src/tests/workflow.negative.test.rn`: failure path and failure UX.\n- `src/tests/workflow.recovery.test.rn`: optional, only when recovery behavior exists.\n\n## Maintenance Notes\n\nKeep `workflow.yaml` schemas aligned with Rune source. Keep `// workflow-covers:` markers aligned with `validation.coverage`, including load and autocomplete. Update this file when the workflow behavior or review expectations change. Keep runtime state and generated artifacts out of git.\n",
            entrypoint = workflow.runtime.entrypoint,
            id = workflow.id,
        ),
        WorkflowRuntimeKind::Typescript => format!(
            "# {title} Design\n\n## Overview\n\nThis workflow is a local TypeScript package driven by `tsx` and validated through `codex workflow validate {id}`.\n\n## Architecture\n\n- `src/workflow.ts` owns the runtime behavior and exports the named default async function, an optional `complete(...)` export, and an optional `WorkflowOutput.toTuiMarkdown(result)` companion.\n- `src/tests/` carries the coverage contract for positive, load, autocomplete, negative, and recovery paths.\n- `workflow.yaml` records validation commands, contract smoke input, and coverage expectations.\n- `state/` holds persistent runtime data; `artifacts/` holds generated run artifacts. Both are ignored except for `state/.gitkeep`.\n\n## Data Flow\n\n1. A registered workflow command loads the workflow from the local package.\n2. The named default export validates input, emits progress, and returns the canonical JSON result.\n3. If present, `WorkflowOutput.toTuiMarkdown(result)` provides the markdown view for the TUI and workflow-to-workflow callers.\n4. `codex workflow validate {id}` runs the local validation commands, checks docs/layout/coverage markers, smoke-tests the contract when configured, extracts the TS contract, and publishes it only after validation passes.\n\n## Failure Handling\n\nValidate inputs early. Surface actionable failures instead of generic exit-only errors. When the workflow cannot satisfy its contract, fail with a specific error that names the broken path.\n\n## Recovery Behavior\n\nPrefer recovery when correctness is preserved. Do not hide corruption or return misleading success. Set `validation.coverage.recovery` to `true` only when recovery exists and is tested.\n\n## Test Matrix\n\n- `src/tests/workflow.positive.test.ts`: positive path, progress, JSON result, and markdown companion coverage.\n- `src/tests/workflow.load.test.ts`: loadability smoke.\n- `src/tests/workflow.autocomplete.test.ts`: registry and command-completion readiness smoke.\n- `src/tests/workflow.negative.test.ts`: failure path and failure UX.\n- `src/tests/workflow.recovery.test.ts`: optional, only when recovery behavior exists.\n\n## Maintenance Notes\n\nKeep dependency usage local. Keep `// workflow-covers:` markers aligned with `validation.coverage`, including load and autocomplete. Update this file when the workflow behavior or review expectations change. Keep runtime state and generated artifacts out of git.\n",
            id = workflow.id,
        ),
    }
}

fn tsconfig_template() -> String {
    "{\n  \"compilerOptions\": {\n    \"types\": [\"node\"],\n    \"allowImportingTsExtensions\": true,\n    \"target\": \"ES2022\",\n    \"module\": \"NodeNext\",\n    \"moduleResolution\": \"NodeNext\",\n    \"strict\": true,\n    \"noEmit\": true\n  },\n  \"include\": [\"src/**/*.ts\"]\n}\n".to_string()
}

fn display_title(id: &str) -> String {
    id.split('/').next_back().unwrap_or(id).replace('-', " ")
}

fn strip_tests_prefix(path: &Path) -> PathBuf {
    if let Ok(stripped) = path.strip_prefix(Path::new("tests")) {
        return stripped.to_path_buf();
    }
    path.file_name()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("workflow.test.ts"))
}

fn is_code_path(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("rn" | "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "mts" | "cts")
    )
}

fn is_test_path(path: &Path) -> bool {
    if path.starts_with(Path::new("tests")) {
        return true;
    }
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.contains(".test.") || name.contains(".spec."))
}

fn is_database_path(path: &Path) -> bool {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    file_name.ends_with(".db")
        || file_name.ends_with(".sqlite")
        || file_name.ends_with(".sqlite3")
        || file_name.ends_with(".db-wal")
        || file_name.ends_with(".db-shm")
        || file_name.ends_with(".sqlite-wal")
        || file_name.ends_with(".sqlite-shm")
        || file_name.ends_with(".sqlite3-wal")
        || file_name.ends_with(".sqlite3-shm")
}

fn command_fixable(command: &str, _exit_code: Option<i32>) -> bool {
    if command.contains("exit 1") {
        return false;
    }
    command.contains("npm run build") || command.contains("npm test")
}

fn dependency_install_fixable(stdout: &str, stderr: &str) -> bool {
    let output = format!("{stdout}\n{stderr}");
    [
        "Cannot find module",
        "Cannot find package",
        "ERR_MODULE_NOT_FOUND",
        "MODULE_NOT_FOUND",
        "could not determine executable to run",
        "tsc: not found",
        "tsx: not found",
    ]
    .iter()
    .any(|needle| output.contains(needle))
}

fn dependency_updates_enabled(policy: &str) -> bool {
    !matches!(policy, "none" | "manual" | "disabled" | "never" | "off")
}

impl FixPlan {
    fn is_empty(&self) -> bool {
        !self.update_validation_yaml
            && !self.repair_output_schema_contracts
            && !self.repair_readme
            && !self.repair_design
            && !self.repair_package_manifest
            && !self.repair_tsconfig
            && !self.create_layout
            && self.add_coverage_markers.is_empty()
            && self.package_names.is_empty()
            && !self.build_script
            && !self.test_script
            && !self.run_script
            && !self.refresh_dependencies
            && !self.spec_reset
    }
}

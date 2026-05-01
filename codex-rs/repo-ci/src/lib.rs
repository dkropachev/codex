use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use codex_protocol::protocol::RepoCiIssueType;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::ExitStatus;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

mod artifactory;
mod branch_diff;
mod ci_artifacts;
mod inference;
mod learning_hints;
mod remote_commit;
mod remote_workflow;
mod repo_ci_ai_learning;
mod runner;

const MANIFEST_VERSION: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AutomationMode {
    Local,
    Remote,
    LocalAndRemote,
}

impl AutomationMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Remote => "remote",
            Self::LocalAndRemote => "local-and-remote",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunMode {
    Prepare,
    Fast,
    Full,
}

impl RunMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prepare => "prepare",
            Self::Fast => "fast",
            Self::Full => "full",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoCiManifest {
    pub version: u32,
    pub repo_root: PathBuf,
    pub repo_key: String,
    pub source_key: String,
    pub automation: AutomationMode,
    pub local_test_time_budget_sec: u64,
    pub learned_at_unix_sec: u64,
    pub learning_sources: Vec<SourceHash>,
    pub inferred_issue_types: Vec<RepoCiIssueType>,
    pub prepare_steps: Vec<RepoCiStep>,
    pub fast_steps: Vec<RepoCiStep>,
    pub full_steps: Vec<RepoCiStep>,
    pub validation: ValidationStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceHash {
    pub path: PathBuf,
    pub sha256: String,
    pub kind: SourceKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    CiWorkflow,
    BuildManifest,
    Lockfile,
    Tooling,
}

impl SourceKind {
    fn artifactory_kind(&self) -> &'static str {
        match self {
            Self::CiWorkflow => "ci_workflow",
            Self::BuildManifest => "build_manifest",
            Self::Lockfile => "lockfile",
            Self::Tooling => "tooling",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoCiStep {
    pub id: String,
    pub command: String,
    pub phase: StepPhase,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepPhase {
    Prepare,
    Lint,
    Build,
    Test,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationStatus {
    NotRun,
    Passed { validated_at_unix_sec: u64 },
    Failed { exit_code: Option<i32> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoCiPaths {
    pub repo_root: PathBuf,
    pub state_dir: PathBuf,
    pub manifest_path: PathBuf,
    pub runner_path: PathBuf,
}

struct ResolvedRepoCiPaths {
    paths: RepoCiPaths,
    learning_sources: Vec<SourceHash>,
    repo_key: String,
    source_key: String,
}

#[derive(Debug, Clone)]
pub struct LearnOptions {
    pub automation: AutomationMode,
    pub local_test_time_budget_sec: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LearnedPlan {
    pub prepare_steps: Vec<RepoCiStep>,
    pub fast_steps: Vec<RepoCiStep>,
    pub full_steps: Vec<RepoCiStep>,
}

pub use branch_diff::BranchDiffSnapshot;
pub use ci_artifacts::RepoCiRunArtifact;
pub use ci_artifacts::RepoCiRunArtifactStatus;
pub use ci_artifacts::RepoCiStepRunStatus;
pub use ci_artifacts::RepoCiStepStatus;
pub use ci_artifacts::lookup_cached_passing_run;
pub use ci_artifacts::manifest_fingerprint;
pub use ci_artifacts::read_run_artifact;
pub use ci_artifacts::run_capture_persisted_with_cancellation;
pub use ci_artifacts::store_captured_run_artifact;
pub use ci_artifacts::worktree_fingerprint;
pub use learning_hints::RepoCiLearningHints;
pub use learning_hints::WorkflowRunHint;
pub use remote_commit::RemoteCommitApplied;
pub use remote_commit::RemoteCommitChangeDetails;
pub use remote_commit::RemoteCommitDecision;
pub use remote_commit::RemoteCommitDecisionContext;
pub use remote_commit::RemoteCommitStrategy;
pub use remote_commit::apply_remote_commit_decision;
pub use remote_commit::fallback_remote_commit_decision;
pub use remote_commit::remote_commit_changed_paths;
pub use remote_commit::remote_commit_decision_context;
pub use remote_commit::remote_commit_decision_schema;
pub use remote_commit::render_remote_commit_decision_prompt;
pub use remote_workflow::RemoteRepoCiCheck;
pub use remote_workflow::RemoteRepoCiWorkflow;
pub use remote_workflow::RemoteRepoCiWorkflowOutcome;
pub use remote_workflow::RemoteRepoCiWorkflowRun;
pub use remote_workflow::RemoteRepoCiWorkflowStart;
pub use remote_workflow::run_remote_workflow;
pub use remote_workflow::run_started_remote_workflow;
pub use remote_workflow::run_started_remote_workflow_with_commit_decision;
pub use remote_workflow::start_remote_workflow;
pub use repo_ci_ai_learning::AI_LEARN_MAX_ATTEMPTS;
pub use repo_ci_ai_learning::RepoCiAiLearnedPlan;
pub use repo_ci_ai_learning::render_repo_ci_learning_prompt;
pub use repo_ci_ai_learning::render_validation_feedback;
pub use repo_ci_ai_learning::repo_ci_ai_plan_schema;
pub use runner::RepoCiCancellation;
use runner::capture_runner;
use runner::run_runner;

#[derive(Debug, Clone)]
pub struct LearnOutcome {
    pub paths: RepoCiPaths,
    pub manifest: RepoCiManifest,
    pub validation_exit_code: Option<i32>,
    pub validation_phase: ValidationPhase,
    pub validation_run: CapturedRun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationPhase {
    Prepare,
    Fast,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusOutcome {
    pub paths: RepoCiPaths,
    pub manifest: Option<RepoCiManifest>,
    pub stale_sources: Vec<SourceHash>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedRun {
    pub status: CapturedExitStatus,
    pub stdout: String,
    pub stderr: String,
    pub steps: Vec<CapturedStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturedStep {
    pub id: String,
    pub event: CapturedStepEvent,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapturedStepEvent {
    Started,
    Finished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturedExitStatus {
    pub code: Option<i32>,
    pub success: bool,
}

impl From<ExitStatus> for CapturedExitStatus {
    fn from(status: ExitStatus) -> Self {
        Self {
            code: status.code(),
            success: status.success(),
        }
    }
}

pub fn repo_root_for_cwd(cwd: &Path) -> Result<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .arg("rev-parse")
        .arg("--show-toplevel")
        .output();
    if let Ok(output) = output
        && output.status.success()
    {
        let root = String::from_utf8(output.stdout)?.trim().to_string();
        if !root.is_empty() {
            return Ok(PathBuf::from(root));
        }
    }
    Ok(cwd.to_path_buf())
}

pub fn paths_for_repo(codex_home: &Path, cwd: &Path) -> Result<RepoCiPaths> {
    Ok(resolve_paths_for_repo(codex_home, cwd)?.paths)
}

fn artifact_sources(sources: &[SourceHash]) -> Vec<artifactory::ArtifactSource> {
    sources
        .iter()
        .map(|source| {
            artifactory::ArtifactSource::new(
                source.path.clone(),
                source.kind.artifactory_kind(),
                source.sha256.clone(),
            )
        })
        .collect()
}

fn resolve_paths_for_repo(codex_home: &Path, cwd: &Path) -> Result<ResolvedRepoCiPaths> {
    let repo_root = repo_root_for_cwd(cwd)?;
    let learning_sources = collect_sources(&repo_root)?;
    let artifactory_sources = artifact_sources(&learning_sources);
    let repo_key = artifactory::repo_key(&repo_root);
    let source_key = artifactory::source_key(&artifactory_sources);
    let state_dir = artifactory::artifact_state_dir_for_keys(codex_home, &repo_key, &source_key);
    let paths = paths_from_state_dir(repo_root, state_dir);
    Ok(ResolvedRepoCiPaths {
        paths,
        learning_sources,
        repo_key,
        source_key,
    })
}

fn paths_from_state_dir(repo_root: PathBuf, state_dir: PathBuf) -> RepoCiPaths {
    RepoCiPaths {
        repo_root,
        manifest_path: state_dir.join("manifest.json"),
        runner_path: state_dir.join("run_ci.sh"),
        state_dir,
    }
}

pub fn learn(codex_home: &Path, cwd: &Path, options: LearnOptions) -> Result<LearnOutcome> {
    let repo_root = repo_root_for_cwd(cwd)?;
    let (prepare_steps, fast_steps, full_steps) = infer_steps(&repo_root)?;
    learn_with_plan(
        codex_home,
        cwd,
        options,
        LearnedPlan {
            prepare_steps,
            fast_steps,
            full_steps,
        },
    )
}

pub fn learn_with_plan(
    codex_home: &Path,
    cwd: &Path,
    options: LearnOptions,
    plan: LearnedPlan,
) -> Result<LearnOutcome> {
    artifactory::prune_stale_artifacts(codex_home)?;
    let resolved = resolve_paths_for_repo(codex_home, cwd)?;
    let paths = resolved.paths;
    fs::create_dir_all(&paths.state_dir)?;
    let mut manifest = RepoCiManifest {
        version: MANIFEST_VERSION,
        repo_key: resolved.repo_key,
        source_key: resolved.source_key,
        repo_root: paths.repo_root.clone(),
        automation: options.automation,
        local_test_time_budget_sec: options.local_test_time_budget_sec,
        learned_at_unix_sec: unix_now(),
        learning_sources: resolved.learning_sources,
        inferred_issue_types: infer_issue_types(&paths.repo_root),
        prepare_steps: plan.prepare_steps,
        fast_steps: plan.fast_steps,
        full_steps: plan.full_steps,
        validation: ValidationStatus::NotRun,
    };
    write_runner(&paths.runner_path, &manifest)?;
    write_manifest(&paths.manifest_path, &manifest)?;
    touch_manifest_artifact_state(codex_home, &paths, &manifest)?;

    let prepare_run = capture_runner(
        &paths,
        "prepare",
        options.local_test_time_budget_sec,
        &RepoCiCancellation::default(),
    )?;
    let (validation_phase, validation_run) = if prepare_run.status.success {
        (
            ValidationPhase::Fast,
            capture_runner(
                &paths,
                "fast",
                options.local_test_time_budget_sec,
                &RepoCiCancellation::default(),
            )?,
        )
    } else {
        (ValidationPhase::Prepare, prepare_run)
    };
    let validation_exit_code = validation_run.status.code;
    manifest.validation = if validation_run.status.success {
        ValidationStatus::Passed {
            validated_at_unix_sec: unix_now(),
        }
    } else {
        ValidationStatus::Failed {
            exit_code: validation_exit_code,
        }
    };
    write_manifest(&paths.manifest_path, &manifest)?;
    touch_manifest_artifact_state(codex_home, &paths, &manifest)?;

    Ok(LearnOutcome {
        paths,
        manifest,
        validation_exit_code,
        validation_phase,
        validation_run,
    })
}

pub fn prepare(codex_home: &Path, cwd: &Path) -> Result<std::process::ExitStatus> {
    artifactory::prune_stale_artifacts(codex_home)?;
    let paths = paths_for_repo(codex_home, cwd)?;
    require_runner(&paths)?;
    let manifest = read_manifest(&paths.manifest_path)?;
    touch_manifest_artifact_state(codex_home, &paths, &manifest)?;
    run_runner(&paths, "prepare", manifest.local_test_time_budget_sec)
}

pub fn run(codex_home: &Path, cwd: &Path, mode: RunMode) -> Result<std::process::ExitStatus> {
    artifactory::prune_stale_artifacts(codex_home)?;
    let paths = paths_for_repo(codex_home, cwd)?;
    require_runner(&paths)?;
    let manifest = read_manifest(&paths.manifest_path)?;
    touch_manifest_artifact_state(codex_home, &paths, &manifest)?;
    run_runner(&paths, mode.as_str(), manifest.local_test_time_budget_sec)
}

pub fn run_capture(codex_home: &Path, cwd: &Path, mode: RunMode) -> Result<CapturedRun> {
    run_capture_with_cancellation(codex_home, cwd, mode, RepoCiCancellation::default())
}

pub fn run_capture_with_cancellation(
    codex_home: &Path,
    cwd: &Path,
    mode: RunMode,
    cancellation: RepoCiCancellation,
) -> Result<CapturedRun> {
    artifactory::prune_stale_artifacts(codex_home)?;
    let paths = paths_for_repo(codex_home, cwd)?;
    require_runner(&paths)?;
    let manifest = read_manifest(&paths.manifest_path)?;
    touch_manifest_artifact_state(codex_home, &paths, &manifest)?;
    capture_runner(
        &paths,
        mode.as_str(),
        manifest.local_test_time_budget_sec,
        &cancellation,
    )
}

pub fn status(codex_home: &Path, cwd: &Path) -> Result<StatusOutcome> {
    artifactory::prune_stale_artifacts(codex_home)?;
    let resolved = resolve_paths_for_repo(codex_home, cwd)?;
    let paths = resolved.paths;
    if paths.manifest_path.exists() {
        let manifest = read_manifest(&paths.manifest_path)?;
        touch_manifest_artifact_state(codex_home, &paths, &manifest)?;
        let stale_sources = changed_sources(&manifest.learning_sources, &resolved.learning_sources);
        return Ok(StatusOutcome {
            paths,
            manifest: Some(manifest),
            stale_sources,
        });
    }

    for state_dir in artifactory::latest_artifact_state_dirs(codex_home, &resolved.repo_key)? {
        let paths = paths_from_state_dir(paths.repo_root.clone(), state_dir);
        if !paths.manifest_path.exists() {
            continue;
        }
        let manifest = read_manifest(&paths.manifest_path)?;
        touch_manifest_artifact_state(codex_home, &paths, &manifest)?;
        let stale_sources = changed_sources(&manifest.learning_sources, &resolved.learning_sources);
        return Ok(StatusOutcome {
            paths,
            manifest: Some(manifest),
            stale_sources,
        });
    }

    Ok(StatusOutcome {
        paths,
        manifest: None,
        stale_sources: Vec::new(),
    })
}

pub fn watch_pr(cwd: &Path) -> Result<std::process::ExitStatus> {
    remote_workflow::watch_pr(cwd)
}

fn require_runner(paths: &RepoCiPaths) -> Result<()> {
    if paths.runner_path.exists() {
        Ok(())
    } else {
        Err(anyhow!(
            "repo CI has not been learned for {}; run `codex repo-ci learn --cwd` first",
            paths.repo_root.display()
        ))
    }
}

fn write_manifest(path: &Path, manifest: &RepoCiManifest) -> Result<()> {
    let data = serde_json::to_vec_pretty(manifest)?;
    fs::write(path, data).with_context(|| format!("failed to write {}", path.display()))
}

fn read_manifest(path: &Path) -> Result<RepoCiManifest> {
    let data = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_slice(&data).with_context(|| format!("failed to parse {}", path.display()))
}

fn register_manifest_artifact_state(
    codex_home: &Path,
    paths: &RepoCiPaths,
    manifest: &RepoCiManifest,
) -> Result<()> {
    artifactory::register_state(
        codex_home,
        &manifest.repo_key,
        &manifest.source_key,
        &paths.state_dir,
        &artifact_sources(&manifest.learning_sources),
        json!({
            "repo_root": &manifest.repo_root,
            "manifest_path": &paths.manifest_path,
            "runner_path": &paths.runner_path,
            "version": manifest.version,
            "automation": manifest.automation,
            "validation": &manifest.validation,
        }),
    )
}

fn touch_manifest_artifact_state(
    codex_home: &Path,
    paths: &RepoCiPaths,
    manifest: &RepoCiManifest,
) -> Result<()> {
    register_manifest_artifact_state(codex_home, paths, manifest)?;
    artifactory::record_artifact_hit(codex_home, &paths.state_dir)
}

fn changed_sources(
    learned_sources: &[SourceHash],
    current_sources: &[SourceHash],
) -> Vec<SourceHash> {
    let learned_by_path = learned_sources
        .iter()
        .map(|source| (source.path.clone(), source))
        .collect::<BTreeMap<_, _>>();
    let current_by_path = current_sources
        .iter()
        .map(|source| (source.path.clone(), source))
        .collect::<BTreeMap<_, _>>();
    artifactory::changed_source_paths(
        &artifact_sources(learned_sources),
        &artifact_sources(current_sources),
    )
    .into_iter()
    .filter_map(|path| {
        learned_by_path
            .get(&path)
            .or_else(|| current_by_path.get(&path))
            .copied()
            .cloned()
    })
    .collect()
}

fn collect_sources(repo_root: &Path) -> Result<Vec<SourceHash>> {
    let mut sources = Vec::new();
    let mut seen_paths = HashSet::new();
    for (relative, kind) in source_candidates(repo_root)? {
        let absolute_path = repo_root.join(&relative);
        let Some(sha256) = hash_file(&absolute_path) else {
            continue;
        };
        if let Ok(canonical_path) = fs::canonicalize(&absolute_path)
            && !seen_paths.insert(canonical_path)
        {
            continue;
        }
        sources.push(SourceHash {
            path: relative,
            sha256,
            kind,
        });
    }
    sources.sort_by(|left, right| left.path.cmp(&right.path));
    sources.dedup_by(|left, right| left.path == right.path);
    Ok(sources)
}

fn source_candidates(repo_root: &Path) -> Result<Vec<(PathBuf, SourceKind)>> {
    let mut candidates = vec![
        (PathBuf::from("Cargo.toml"), SourceKind::BuildManifest),
        (PathBuf::from("Cargo.lock"), SourceKind::Lockfile),
        (PathBuf::from("rust-toolchain"), SourceKind::Tooling),
        (PathBuf::from("rust-toolchain.toml"), SourceKind::Tooling),
        (PathBuf::from("justfile"), SourceKind::Tooling),
        (PathBuf::from("Justfile"), SourceKind::Tooling),
        (PathBuf::from("Makefile"), SourceKind::Tooling),
        (PathBuf::from("makefile"), SourceKind::Tooling),
        (PathBuf::from("GNUmakefile"), SourceKind::Tooling),
        (PathBuf::from("package.json"), SourceKind::BuildManifest),
        (PathBuf::from("package-lock.json"), SourceKind::Lockfile),
        (PathBuf::from("pnpm-lock.yaml"), SourceKind::Lockfile),
        (PathBuf::from("yarn.lock"), SourceKind::Lockfile),
        (PathBuf::from("pyproject.toml"), SourceKind::BuildManifest),
        (PathBuf::from("requirements.txt"), SourceKind::BuildManifest),
        (PathBuf::from("uv.lock"), SourceKind::Lockfile),
        (PathBuf::from("tox.ini"), SourceKind::Tooling),
        (PathBuf::from("pytest.ini"), SourceKind::Tooling),
        (PathBuf::from("build.sbt"), SourceKind::BuildManifest),
        (PathBuf::from(".scalafmt.conf"), SourceKind::Tooling),
        (
            PathBuf::from("project/build.properties"),
            SourceKind::Tooling,
        ),
        (PathBuf::from("project/plugins.sbt"), SourceKind::Tooling),
    ];
    let workflow_dir = repo_root.join(".github/workflows");
    if workflow_dir.is_dir() {
        for entry in fs::read_dir(&workflow_dir)? {
            let entry = entry?;
            let path = entry.path();
            let Some(ext) = path.extension().and_then(OsStr::to_str) else {
                continue;
            };
            if matches!(ext, "yml" | "yaml")
                && let Ok(relative) = path.strip_prefix(repo_root)
            {
                candidates.push((relative.to_path_buf(), SourceKind::CiWorkflow));
            }
        }
    }
    Ok(candidates)
}

fn infer_steps(repo_root: &Path) -> Result<(Vec<RepoCiStep>, Vec<RepoCiStep>, Vec<RepoCiStep>)> {
    inference::infer_steps(repo_root)
}

/// Collect prompt scaffolding for AI-based repo CI discovery.
pub fn collect_learning_hints(repo_root: &Path) -> Result<RepoCiLearningHints> {
    learning_hints::collect_learning_hints(repo_root)
}

pub fn default_issue_types() -> Vec<RepoCiIssueType> {
    vec![
        RepoCiIssueType::Correctness,
        RepoCiIssueType::Reliability,
        RepoCiIssueType::Maintainability,
    ]
}

fn infer_issue_types(repo_root: &Path) -> Vec<RepoCiIssueType> {
    let mut issue_types = default_issue_types();
    if has_file(repo_root, "Dockerfile")
        || has_file(repo_root, "docker-compose.yml")
        || has_file(repo_root, "docker-compose.yaml")
        || repo_root.join("k8s").is_dir()
        || repo_root.join("helm").is_dir()
    {
        issue_types.push(RepoCiIssueType::Scalability);
        issue_types.push(RepoCiIssueType::Observability);
    }
    if has_file(repo_root, "package.json") {
        issue_types.push(RepoCiIssueType::UxConfigCli);
        issue_types.push(RepoCiIssueType::Compatibility);
    }
    if has_file(repo_root, "Cargo.toml") || has_file(repo_root, "go.mod") {
        issue_types.push(RepoCiIssueType::Performance);
    }
    if has_file(repo_root, "pytest.ini")
        || has_file(repo_root, "pyproject.toml")
        || has_file(repo_root, "Cargo.toml")
    {
        issue_types.push(RepoCiIssueType::Testability);
    }
    if repo_root.join(".github/workflows").is_dir()
        || has_file(repo_root, "Dockerfile")
        || has_file(repo_root, "terraform.lock.hcl")
    {
        issue_types.push(RepoCiIssueType::Security);
    }
    issue_types.sort();
    issue_types.dedup();
    issue_types
}

pub(crate) fn add_just_steps(
    justfile: &str,
    prepare: &mut Vec<RepoCiStep>,
    fast: &mut Vec<RepoCiStep>,
    full: &mut Vec<RepoCiStep>,
) {
    if justfile_has_recipe(justfile, "setup") {
        prepare.push(step("just-setup", "just setup", StepPhase::Prepare));
    }
    if justfile_has_recipe(justfile, "prepare") {
        prepare.push(step("just-prepare", "just prepare", StepPhase::Prepare));
    }
    for (recipe, phase) in [
        ("fmt", StepPhase::Lint),
        ("format", StepPhase::Lint),
        ("lint", StepPhase::Lint),
        ("clippy", StepPhase::Lint),
        ("build", StepPhase::Build),
        ("test", StepPhase::Test),
    ] {
        if justfile_has_recipe(justfile, recipe) {
            let ci_step = step(
                &format!("just-{recipe}"),
                &format!("just {recipe}"),
                phase.clone(),
            );
            fast.push(ci_step.clone());
            full.push(ci_step);
        }
    }
    for recipe in ["integration", "e2e", "ui-test", "ui-tests"] {
        if justfile_has_recipe(justfile, recipe) {
            full.push(step(
                &format!("just-{recipe}"),
                &format!("just {recipe}"),
                StepPhase::Test,
            ));
        }
    }
}

pub(crate) fn add_node_steps(
    repo_root: &Path,
    prepare: &mut Vec<RepoCiStep>,
    fast: &mut Vec<RepoCiStep>,
    full: &mut Vec<RepoCiStep>,
) {
    if has_file(repo_root, "pnpm-lock.yaml") {
        prepare.push(step(
            "pnpm-install",
            "pnpm install --frozen-lockfile",
            StepPhase::Prepare,
        ));
    } else if has_file(repo_root, "package-lock.json") {
        prepare.push(step("npm-ci", "npm ci", StepPhase::Prepare));
    } else if has_file(repo_root, "yarn.lock") {
        prepare.push(step(
            "yarn-install",
            "yarn install --frozen-lockfile",
            StepPhase::Prepare,
        ));
    }

    let Some(package_json) = read_optional(repo_root, "package.json").ok().flatten() else {
        return;
    };
    for (script, phase) in [
        ("lint", StepPhase::Lint),
        ("build", StepPhase::Build),
        ("test", StepPhase::Test),
    ] {
        if package_json.contains(&format!("\"{script}\"")) {
            let cmd = if has_file(repo_root, "pnpm-lock.yaml") {
                format!("pnpm {script}")
            } else if has_file(repo_root, "yarn.lock") {
                format!("yarn {script}")
            } else {
                format!("npm run {script}")
            };
            let ci_step = step(&format!("node-{script}"), &cmd, phase.clone());
            fast.push(ci_step.clone());
            full.push(ci_step);
        }
    }
}

pub(crate) fn add_python_steps(
    repo_root: &Path,
    prepare: &mut Vec<RepoCiStep>,
    fast: &mut Vec<RepoCiStep>,
    full: &mut Vec<RepoCiStep>,
) {
    if has_file(repo_root, "uv.lock") {
        prepare.push(step("uv-sync", "uv sync --frozen", StepPhase::Prepare));
    } else if has_file(repo_root, "requirements.txt") {
        prepare.push(step(
            "python-venv",
            "python3 -m venv .venv && . .venv/bin/activate && pip install -r requirements.txt",
            StepPhase::Prepare,
        ));
    }
    let pytest_cmd = if has_file(repo_root, "uv.lock") {
        "uv run pytest"
    } else {
        ". .venv/bin/activate 2>/dev/null || true; pytest"
    };
    if has_file(repo_root, "pytest.ini") || has_file(repo_root, "pyproject.toml") {
        let ci_step = step("python-pytest", pytest_cmd, StepPhase::Test);
        fast.push(ci_step.clone());
        full.push(ci_step);
    }
}

pub(crate) fn justfile_has_recipe(justfile: &str, recipe: &str) -> bool {
    let prefix = format!("{recipe}:");
    justfile
        .lines()
        .any(|line| line.starts_with(&prefix) || line.starts_with(&format!("@{prefix}")))
}

pub(crate) fn step(id: &str, command: &str, phase: StepPhase) -> RepoCiStep {
    RepoCiStep {
        id: id.to_string(),
        command: command.to_string(),
        phase,
    }
}

pub(crate) fn read_optional(repo_root: &Path, relative: &str) -> Result<Option<String>> {
    let path = repo_root.join(relative);
    if path.exists() {
        fs::read_to_string(&path)
            .map(Some)
            .with_context(|| format!("failed to read {}", path.display()))
    } else {
        Ok(None)
    }
}

pub(crate) fn has_file(repo_root: &Path, relative: &str) -> bool {
    repo_root.join(relative).is_file()
}

fn hash_file(path: &Path) -> Option<String> {
    codex_artifactory::file_sha256(path)
}

fn write_runner(path: &Path, manifest: &RepoCiManifest) -> Result<()> {
    let mut script =
        String::from("#!/usr/bin/env bash\nset -euo pipefail\n\nmode=\"${1:-fast}\"\nrepo_root=");
    script.push_str(&shell_quote(&manifest.repo_root.to_string_lossy()));
    script.push_str("\nrepo_root=\"${CODEX_REPO_CI_REPO_ROOT:-$repo_root}\"");
    script.push_str("\ncd \"$repo_root\"\n\nrecord_step() {\n  if [[ -n \"${CODEX_REPO_CI_JSONL:-}\" ]]; then\n    local id_json=\"$1\"\n    id_json=\"${id_json//\\\\/\\\\\\\\}\"\n    id_json=\"${id_json//\\\"/\\\\\\\"}\"\n    printf '{\"id\":\"%s\",\"event\":\"%s\",\"exit_code\":%s}\\n' \"$id_json\" \"$2\" \"$3\" >> \"$CODEX_REPO_CI_JSONL\"\n  fi\n}\n\nrun_step() {\n  local id=\"$1\"\n  shift\n  echo \"==> ${id}\"\n  record_step \"$id\" started null\n  set +e\n  \"$@\"\n  local status=$?\n  set -e\n  record_step \"$id\" finished \"$status\"\n  return \"$status\"\n}\n\nprepare() {\n");
    if manifest.prepare_steps.is_empty() {
        script.push_str("  :\n");
    }
    for step in &manifest.prepare_steps {
        push_script_step(&mut script, step);
    }
    script.push_str("}\n\nfast() {\n  prepare\n");
    if manifest.fast_steps.is_empty() {
        script.push_str("  :\n");
    }
    for step in &manifest.fast_steps {
        push_script_step(&mut script, step);
    }
    script.push_str("}\n\nfull() {\n  prepare\n");
    if manifest.full_steps.is_empty() {
        script.push_str("  :\n");
    }
    for step in &manifest.full_steps {
        push_script_step(&mut script, step);
    }
    script.push_str("}\n\ncase \"$mode\" in\n  prepare) prepare ;;\n  fast) fast ;;\n  full) full ;;\n  *) echo \"usage: $0 {prepare|fast|full}\" >&2; exit 64 ;;\nesac\n");

    let mut file =
        fs::File::create(path).with_context(|| format!("failed to create {}", path.display()))?;
    file.write_all(script.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = file.metadata()?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

fn push_script_step(script: &mut String, step: &RepoCiStep) {
    script.push_str("  run_step ");
    script.push_str(&shell_quote(&step.id));
    script.push_str(" bash -lc ");
    script.push_str(&shell_quote(&step.command));
    script.push('\n');
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[cfg(unix)]
    fn assert_process_exits(child_pid: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            let output = std::process::Command::new("ps")
                .arg("-o")
                .arg("stat=")
                .arg("-p")
                .arg(child_pid)
                .output()
                .expect("ps");
            let process_state = String::from_utf8_lossy(&output.stdout);
            if !output.status.success() || process_state.trim_start().starts_with('Z') {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("descendant process {child_pid} survived repo CI cleanup");
    }

    #[test]
    fn learns_justfile_steps() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(
            temp.path().join("justfile"),
            "fmt:\n\tcargo fmt\n\nclippy:\n\tcargo clippy\n\ntest:\n\tcargo test\n",
        )
        .expect("write justfile");

        let (_prepare, fast, full) = infer_steps(temp.path()).expect("infer steps");

        assert_eq!(
            fast,
            vec![
                step("just-fmt", "just fmt", StepPhase::Lint),
                step("just-clippy", "just clippy", StepPhase::Lint),
                step("just-test", "just test", StepPhase::Test),
            ]
        );
        assert_eq!(full, fast);
    }

    #[test]
    fn learns_makefile_steps_for_scala_repo() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(
            temp.path().join("Makefile"),
            "lint:\n\t@echo lint\n\nbuild:\n\t@echo build\n\ntest-unit:\n\t@echo unit\n\ntest-integration:\n\t@echo integration\n",
        )
        .expect("write makefile");
        fs::write(temp.path().join("build.sbt"), "lazy val root = project").expect("write sbt");

        let (prepare, fast, full) = infer_steps(temp.path()).expect("infer steps");

        assert_eq!(prepare, Vec::<RepoCiStep>::new());
        assert_eq!(
            fast,
            vec![
                step("make-lint", "make lint", StepPhase::Lint),
                step("make-build", "make build", StepPhase::Build),
                step("make-test-unit", "make test-unit", StepPhase::Test),
            ]
        );
        assert_eq!(
            full,
            vec![
                step("make-lint", "make lint", StepPhase::Lint),
                step("make-build", "make build", StepPhase::Build),
                step("make-test-unit", "make test-unit", StepPhase::Test),
                step(
                    "make-test-integration",
                    "make test-integration",
                    StepPhase::Test,
                ),
            ]
        );
    }

    #[test]
    fn learns_sbt_steps_without_makefile() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(temp.path().join("build.sbt"), "lazy val root = project").expect("write sbt");
        fs::write(temp.path().join(".scalafmt.conf"), "version=3.0.0").expect("write scalafmt");

        let (prepare, fast, full) = infer_steps(temp.path()).expect("infer steps");

        assert_eq!(prepare, Vec::<RepoCiStep>::new());
        assert_eq!(
            fast,
            vec![
                step(
                    "sbt-scalafmt-check",
                    "sbt scalafmtCheckAll",
                    StepPhase::Lint
                ),
                step("sbt-compile", "sbt compile", StepPhase::Build),
                step("sbt-test", "sbt test", StepPhase::Test),
            ]
        );
        assert_eq!(full, fast);
    }

    #[test]
    fn collects_makefile_and_sbt_sources() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(temp.path().join("Makefile"), "lint:\n\t@true\n").expect("write makefile");
        fs::write(temp.path().join("build.sbt"), "lazy val root = project").expect("write sbt");
        fs::write(temp.path().join(".scalafmt.conf"), "version=3.0.0").expect("write scalafmt");
        fs::create_dir_all(temp.path().join("project")).expect("create project dir");
        fs::write(temp.path().join("project/plugins.sbt"), "addSbtPlugin(...)")
            .expect("write plugins");

        let sources = collect_sources(temp.path()).expect("collect sources");
        let paths = sources
            .into_iter()
            .map(|source| source.path)
            .collect::<Vec<_>>();

        assert_eq!(
            paths,
            vec![
                PathBuf::from(".scalafmt.conf"),
                PathBuf::from("Makefile"),
                PathBuf::from("build.sbt"),
                PathBuf::from("project/plugins.sbt"),
            ]
        );
    }

    #[test]
    fn learns_with_generated_plan_and_captures_validation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let codex_home = temp.path().join("codex-home");
        let repo = temp.path().join("repo");
        fs::create_dir(&repo).expect("create repo");

        let outcome = learn_with_plan(
            &codex_home,
            &repo,
            LearnOptions {
                automation: AutomationMode::Local,
                local_test_time_budget_sec: 300,
            },
            LearnedPlan {
                prepare_steps: vec![],
                fast_steps: vec![step("ok", "true", StepPhase::Test)],
                full_steps: vec![step("ok", "true", StepPhase::Test)],
            },
        )
        .expect("learn with plan");

        assert_eq!(outcome.validation_phase, ValidationPhase::Fast);
        assert_eq!(outcome.validation_exit_code, Some(0));
        assert!(matches!(
            outcome.manifest.validation,
            ValidationStatus::Passed { .. }
        ));
        assert_eq!(
            outcome.validation_run.steps,
            vec![
                CapturedStep {
                    id: "ok".to_string(),
                    event: CapturedStepEvent::Started,
                    exit_code: None,
                },
                CapturedStep {
                    id: "ok".to_string(),
                    event: CapturedStepEvent::Finished,
                    exit_code: Some(0),
                },
            ]
        );
    }

    #[test]
    fn learn_with_generated_plan_reports_prepare_failures() {
        let temp = tempfile::tempdir().expect("tempdir");
        let codex_home = temp.path().join("codex-home");
        let repo = temp.path().join("repo");
        fs::create_dir(&repo).expect("create repo");

        let outcome = learn_with_plan(
            &codex_home,
            &repo,
            LearnOptions {
                automation: AutomationMode::Local,
                local_test_time_budget_sec: 300,
            },
            LearnedPlan {
                prepare_steps: vec![step("nope", "false", StepPhase::Prepare)],
                fast_steps: vec![step("ok", "true", StepPhase::Test)],
                full_steps: vec![step("ok", "true", StepPhase::Test)],
            },
        )
        .expect("learn with plan");

        assert_eq!(outcome.validation_phase, ValidationPhase::Prepare);
        assert_eq!(outcome.validation_exit_code, Some(1));
        assert_eq!(
            outcome.manifest.validation,
            ValidationStatus::Failed { exit_code: Some(1) }
        );
        assert_eq!(
            outcome.validation_run.steps,
            vec![
                CapturedStep {
                    id: "nope".to_string(),
                    event: CapturedStepEvent::Started,
                    exit_code: None,
                },
                CapturedStep {
                    id: "nope".to_string(),
                    event: CapturedStepEvent::Finished,
                    exit_code: Some(1),
                },
            ]
        );
    }

    #[test]
    fn tracks_changed_source_hashes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let codex_home = temp.path().join("codex-home");
        let repo = temp.path().join("repo");
        fs::create_dir(&repo).expect("create repo");
        fs::write(repo.join("Cargo.toml"), "[package]\nname = \"x\"\n").expect("write cargo");
        let paths = paths_for_repo(&codex_home, &repo).expect("paths");
        fs::create_dir_all(&paths.state_dir).expect("state dir");
        let manifest = RepoCiManifest {
            version: MANIFEST_VERSION,
            repo_root: repo.clone(),
            repo_key: artifactory::repo_key(&repo),
            source_key: artifactory::source_key(&artifact_sources(
                &collect_sources(&repo).expect("sources"),
            )),
            automation: AutomationMode::LocalAndRemote,
            local_test_time_budget_sec: 300,
            learned_at_unix_sec: 1,
            learning_sources: collect_sources(&repo).expect("sources"),
            inferred_issue_types: default_issue_types(),
            prepare_steps: vec![],
            fast_steps: vec![],
            full_steps: vec![],
            validation: ValidationStatus::NotRun,
        };
        write_manifest(&paths.manifest_path, &manifest).expect("write manifest");
        register_manifest_artifact_state(&codex_home, &paths, &manifest).expect("register state");

        fs::write(repo.join("Cargo.toml"), "[package]\nname = \"y\"\n").expect("update cargo");

        let status = status(&codex_home, &repo).expect("status");
        assert_eq!(status.stale_sources, manifest.learning_sources);
    }

    #[test]
    fn tracks_added_source_hashes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let codex_home = temp.path().join("codex-home");
        let repo = temp.path().join("repo");
        fs::create_dir(&repo).expect("create repo");
        fs::write(repo.join("Cargo.toml"), "[package]\nname = \"x\"\n").expect("write cargo");
        let paths = paths_for_repo(&codex_home, &repo).expect("paths");
        fs::create_dir_all(&paths.state_dir).expect("state dir");
        let manifest = RepoCiManifest {
            version: MANIFEST_VERSION,
            repo_root: repo.clone(),
            repo_key: artifactory::repo_key(&repo),
            source_key: artifactory::source_key(&artifact_sources(
                &collect_sources(&repo).expect("sources"),
            )),
            automation: AutomationMode::LocalAndRemote,
            local_test_time_budget_sec: 300,
            learned_at_unix_sec: 1,
            learning_sources: collect_sources(&repo).expect("sources"),
            inferred_issue_types: default_issue_types(),
            prepare_steps: vec![],
            fast_steps: vec![],
            full_steps: vec![],
            validation: ValidationStatus::NotRun,
        };
        write_manifest(&paths.manifest_path, &manifest).expect("write manifest");
        register_manifest_artifact_state(&codex_home, &paths, &manifest).expect("register state");

        fs::write(repo.join("Cargo.lock"), "# lock\n").expect("write lockfile");

        let status = status(&codex_home, &repo).expect("status");
        let paths = status
            .stale_sources
            .into_iter()
            .map(|source| source.path)
            .collect::<Vec<_>>();
        assert_eq!(paths, vec![PathBuf::from("Cargo.lock")]);
    }

    #[test]
    fn tracks_removed_source_hashes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let codex_home = temp.path().join("codex-home");
        let repo = temp.path().join("repo");
        fs::create_dir(&repo).expect("create repo");
        fs::write(repo.join("Cargo.toml"), "[package]\nname = \"x\"\n").expect("write cargo");
        fs::write(repo.join("Cargo.lock"), "# lock\n").expect("write lockfile");
        let paths = paths_for_repo(&codex_home, &repo).expect("paths");
        fs::create_dir_all(&paths.state_dir).expect("state dir");
        let learning_sources = collect_sources(&repo).expect("sources");
        let manifest = RepoCiManifest {
            version: MANIFEST_VERSION,
            repo_root: repo.clone(),
            repo_key: artifactory::repo_key(&repo),
            source_key: artifactory::source_key(&artifact_sources(&learning_sources)),
            automation: AutomationMode::LocalAndRemote,
            local_test_time_budget_sec: 300,
            learned_at_unix_sec: 1,
            learning_sources,
            inferred_issue_types: default_issue_types(),
            prepare_steps: vec![],
            fast_steps: vec![],
            full_steps: vec![],
            validation: ValidationStatus::NotRun,
        };
        write_manifest(&paths.manifest_path, &manifest).expect("write manifest");
        register_manifest_artifact_state(&codex_home, &paths, &manifest).expect("register state");

        fs::remove_file(repo.join("Cargo.lock")).expect("remove lockfile");

        let status = status(&codex_home, &repo).expect("status");
        let paths = status
            .stale_sources
            .into_iter()
            .map(|source| source.path)
            .collect::<Vec<_>>();
        assert_eq!(paths, vec![PathBuf::from("Cargo.lock")]);
    }

    #[test]
    fn status_updates_last_hit_when_using_stale_source_artifact() {
        let temp = tempfile::tempdir().expect("tempdir");
        let codex_home = temp.path().join("codex-home");
        let repo = temp.path().join("repo");
        fs::create_dir(&repo).expect("create repo");
        fs::write(repo.join("Cargo.toml"), "[package]\nname = \"x\"\n").expect("write cargo");
        let paths = paths_for_repo(&codex_home, &repo).expect("paths");
        fs::create_dir_all(&paths.state_dir).expect("state dir");
        let learning_sources = collect_sources(&repo).expect("sources");
        let manifest = RepoCiManifest {
            version: MANIFEST_VERSION,
            repo_root: repo.clone(),
            repo_key: artifactory::repo_key(&repo),
            source_key: artifactory::source_key(&artifact_sources(&learning_sources)),
            automation: AutomationMode::LocalAndRemote,
            local_test_time_budget_sec: 300,
            learned_at_unix_sec: 1,
            learning_sources,
            inferred_issue_types: default_issue_types(),
            prepare_steps: vec![],
            fast_steps: vec![],
            full_steps: vec![],
            validation: ValidationStatus::NotRun,
        };
        write_manifest(&paths.manifest_path, &manifest).expect("write manifest");
        register_manifest_artifact_state(&codex_home, &paths, &manifest).expect("register state");
        assert_eq!(
            artifactory::artifact_last_hit_unix_sec(&codex_home, &paths.state_dir)
                .expect("last hit"),
            None
        );

        fs::write(repo.join("Cargo.toml"), "[package]\nname = \"y\"\n").expect("update cargo");

        let status = status(&codex_home, &repo).expect("status");
        assert!(status.manifest.is_some());
        assert!(
            artifactory::artifact_last_hit_unix_sec(&codex_home, &paths.state_dir)
                .expect("last hit")
                .is_some()
        );
    }

    #[test]
    fn status_ignores_legacy_commit_keyed_artifacts() {
        let temp = tempfile::tempdir().expect("tempdir");
        let codex_home = temp.path().join("codex-home");
        let repo = temp.path().join("repo");
        fs::create_dir(&repo).expect("create repo");
        fs::write(repo.join("Cargo.toml"), "[package]\nname = \"x\"\n").expect("write cargo");
        let repo_key = artifactory::repo_key(&repo);
        let legacy_dir = artifactory::repo_artifacts_dir(&codex_home, &repo_key);
        fs::create_dir_all(&legacy_dir).expect("legacy dir");
        fs::write(legacy_dir.join("manifest.json"), "not json\n").expect("legacy manifest");

        let status = status(&codex_home, &repo).expect("status");

        assert!(status.manifest.is_none());
    }

    #[test]
    fn paths_for_repo_does_not_record_artifact_hits() {
        let temp = tempfile::tempdir().expect("tempdir");
        let codex_home = temp.path().join("codex-home");
        let repo = temp.path().join("repo");
        fs::create_dir(&repo).expect("create repo");
        fs::write(repo.join("Cargo.toml"), "[package]\nname = \"x\"\n").expect("write cargo");
        let paths = paths_for_repo(&codex_home, &repo).expect("paths");
        fs::create_dir_all(&paths.state_dir).expect("state dir");

        let _ = paths_for_repo(&codex_home, &repo).expect("paths");

        assert_eq!(
            artifactory::artifact_last_hit_unix_sec(&codex_home, &paths.state_dir)
                .expect("last hit"),
            None
        );
    }

    #[test]
    fn capture_runner_records_step_jsonl() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        let state_dir = temp.path().join("state");
        fs::create_dir(&repo).expect("create repo");
        fs::create_dir(&state_dir).expect("create state");
        let paths = RepoCiPaths {
            repo_root: repo.clone(),
            state_dir,
            manifest_path: temp.path().join("manifest.json"),
            runner_path: temp.path().join("run_ci.sh"),
        };
        let manifest = RepoCiManifest {
            version: MANIFEST_VERSION,
            repo_root: repo,
            repo_key: "repo".to_string(),
            source_key: "source".to_string(),
            automation: AutomationMode::Local,
            local_test_time_budget_sec: 300,
            learned_at_unix_sec: 1,
            learning_sources: vec![],
            inferred_issue_types: default_issue_types(),
            prepare_steps: vec![],
            fast_steps: vec![step("ok", "true", StepPhase::Test)],
            full_steps: vec![],
            validation: ValidationStatus::NotRun,
        };
        write_runner(&paths.runner_path, &manifest).expect("write runner");

        let run = capture_runner(
            &paths,
            "fast",
            manifest.local_test_time_budget_sec,
            &RepoCiCancellation::default(),
        )
        .expect("capture");

        assert!(
            run.status.success,
            "stdout:\n{}\nstderr:\n{}",
            run.stdout, run.stderr
        );
        assert_eq!(
            run.steps,
            vec![
                CapturedStep {
                    id: "ok".to_string(),
                    event: CapturedStepEvent::Started,
                    exit_code: None,
                },
                CapturedStep {
                    id: "ok".to_string(),
                    event: CapturedStepEvent::Finished,
                    exit_code: Some(0),
                },
            ]
        );
    }

    #[test]
    fn capture_runner_times_out_and_records_started_step() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        let state_dir = temp.path().join("state");
        fs::create_dir(&repo).expect("create repo");
        fs::create_dir(&state_dir).expect("create state");
        let paths = RepoCiPaths {
            repo_root: repo.clone(),
            state_dir,
            manifest_path: temp.path().join("manifest.json"),
            runner_path: temp.path().join("run_ci.sh"),
        };
        let manifest = RepoCiManifest {
            version: MANIFEST_VERSION,
            repo_root: repo,
            repo_key: "repo".to_string(),
            source_key: "source".to_string(),
            automation: AutomationMode::Local,
            local_test_time_budget_sec: 1,
            learned_at_unix_sec: 1,
            learning_sources: vec![],
            inferred_issue_types: default_issue_types(),
            prepare_steps: vec![],
            fast_steps: vec![step("slow", "sleep 30", StepPhase::Test)],
            full_steps: vec![],
            validation: ValidationStatus::NotRun,
        };
        write_runner(&paths.runner_path, &manifest).expect("write runner");

        let run = capture_runner(
            &paths,
            "fast",
            manifest.local_test_time_budget_sec,
            &RepoCiCancellation::default(),
        )
        .expect("capture");

        assert_eq!(
            run.status,
            CapturedExitStatus {
                code: None,
                success: false
            }
        );
        assert!(
            run.stderr.contains("repo CI fast timed out after 1s"),
            "{}",
            run.stderr
        );
        assert_eq!(
            run.steps,
            vec![CapturedStep {
                id: "slow".to_string(),
                event: CapturedStepEvent::Started,
                exit_code: None,
            }]
        );
    }

    #[cfg(unix)]
    #[test]
    fn capture_runner_timeout_kills_descendant_processes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        let state_dir = temp.path().join("state");
        fs::create_dir(&repo).expect("create repo");
        fs::create_dir(&state_dir).expect("create state");
        let child_pid_path = repo.join("child.pid");
        let slow_command = format!("sleep 30 & echo $! > {} ; wait", child_pid_path.display());
        let paths = RepoCiPaths {
            repo_root: repo.clone(),
            state_dir,
            manifest_path: temp.path().join("manifest.json"),
            runner_path: temp.path().join("run_ci.sh"),
        };
        let manifest = RepoCiManifest {
            version: MANIFEST_VERSION,
            repo_root: repo,
            repo_key: "repo".to_string(),
            source_key: "source".to_string(),
            automation: AutomationMode::Local,
            local_test_time_budget_sec: 1,
            learned_at_unix_sec: 1,
            learning_sources: vec![],
            inferred_issue_types: default_issue_types(),
            prepare_steps: vec![],
            fast_steps: vec![step("slow", &slow_command, StepPhase::Test)],
            full_steps: vec![],
            validation: ValidationStatus::NotRun,
        };
        write_runner(&paths.runner_path, &manifest).expect("write runner");

        let run = capture_runner(
            &paths,
            "fast",
            manifest.local_test_time_budget_sec,
            &RepoCiCancellation::default(),
        )
        .expect("capture");

        assert!(!run.status.success);
        let child_pid = fs::read_to_string(&child_pid_path)
            .expect("read child pid")
            .trim()
            .to_string();
        assert_process_exits(&child_pid);
    }

    #[cfg(unix)]
    #[test]
    fn capture_runner_cancellation_kills_descendant_processes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        let state_dir = temp.path().join("state");
        fs::create_dir(&repo).expect("create repo");
        fs::create_dir(&state_dir).expect("create state");
        let child_pid_path = repo.join("child.pid");
        let slow_command = format!("sleep 30 & echo $! > {} ; wait", child_pid_path.display());
        let paths = RepoCiPaths {
            repo_root: repo.clone(),
            state_dir,
            manifest_path: temp.path().join("manifest.json"),
            runner_path: temp.path().join("run_ci.sh"),
        };
        let manifest = RepoCiManifest {
            version: MANIFEST_VERSION,
            repo_root: repo,
            repo_key: "repo".to_string(),
            source_key: "source".to_string(),
            automation: AutomationMode::Local,
            local_test_time_budget_sec: 300,
            learned_at_unix_sec: 1,
            learning_sources: vec![],
            inferred_issue_types: default_issue_types(),
            prepare_steps: vec![],
            fast_steps: vec![step("slow", &slow_command, StepPhase::Test)],
            full_steps: vec![],
            validation: ValidationStatus::NotRun,
        };
        write_runner(&paths.runner_path, &manifest).expect("write runner");

        let cancellation = RepoCiCancellation::default();
        let cancellation_for_thread = cancellation.clone();
        let child_pid_path_for_thread = child_pid_path.clone();
        let cancel_thread = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while std::time::Instant::now() < deadline {
                if child_pid_path_for_thread.exists() {
                    cancellation_for_thread.cancel();
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            cancellation_for_thread.cancel();
        });
        let run = capture_runner(
            &paths,
            "fast",
            manifest.local_test_time_budget_sec,
            &cancellation,
        )
        .expect("capture");
        cancel_thread.join().expect("cancel thread");

        assert!(!run.status.success);
        assert!(
            run.stderr.contains("repo CI fast was cancelled"),
            "{}",
            run.stderr
        );
        let child_pid = fs::read_to_string(&child_pid_path)
            .expect("read child pid")
            .trim()
            .to_string();
        assert_process_exits(&child_pid);
    }

    #[test]
    fn shared_runner_uses_current_repo_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        let learned_repo = temp.path().join("learned");
        let current_repo = temp.path().join("current");
        let state_dir = temp.path().join("state");
        fs::create_dir(&learned_repo).expect("create learned repo");
        fs::create_dir(&current_repo).expect("create current repo");
        fs::create_dir(&state_dir).expect("create state");
        fs::write(current_repo.join("marker"), "").expect("write marker");
        let paths = RepoCiPaths {
            repo_root: current_repo,
            state_dir,
            manifest_path: temp.path().join("manifest.json"),
            runner_path: temp.path().join("run_ci.sh"),
        };
        let manifest = RepoCiManifest {
            version: MANIFEST_VERSION,
            repo_root: learned_repo,
            repo_key: "repo".to_string(),
            source_key: "source".to_string(),
            automation: AutomationMode::Local,
            local_test_time_budget_sec: 300,
            learned_at_unix_sec: 1,
            learning_sources: vec![],
            inferred_issue_types: default_issue_types(),
            prepare_steps: vec![],
            fast_steps: vec![step("current-root", "test -f marker", StepPhase::Test)],
            full_steps: vec![],
            validation: ValidationStatus::NotRun,
        };
        write_runner(&paths.runner_path, &manifest).expect("write runner");

        let run = capture_runner(
            &paths,
            "fast",
            manifest.local_test_time_budget_sec,
            &RepoCiCancellation::default(),
        )
        .expect("capture");

        assert!(
            run.status.success,
            "stdout:\n{}\nstderr:\n{}",
            run.stdout, run.stderr
        );
    }

    #[test]
    fn infer_issue_types_adds_repo_specific_categories() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(
            temp.path().join("Cargo.toml"),
            "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
        )
        .expect("write cargo");
        fs::write(temp.path().join("package.json"), "{ \"name\": \"x\" }").expect("write package");
        fs::write(temp.path().join("Dockerfile"), "FROM scratch").expect("write dockerfile");
        fs::create_dir_all(temp.path().join(".github/workflows")).expect("workflow dir");

        let issue_types = infer_issue_types(temp.path());

        assert!(issue_types.contains(&RepoCiIssueType::Correctness));
        assert!(issue_types.contains(&RepoCiIssueType::Reliability));
        assert!(issue_types.contains(&RepoCiIssueType::Maintainability));
        assert!(issue_types.contains(&RepoCiIssueType::Performance));
        assert!(issue_types.contains(&RepoCiIssueType::Testability));
        assert!(issue_types.contains(&RepoCiIssueType::UxConfigCli));
        assert!(issue_types.contains(&RepoCiIssueType::Compatibility));
        assert!(issue_types.contains(&RepoCiIssueType::Scalability));
        assert!(issue_types.contains(&RepoCiIssueType::Observability));
        assert!(issue_types.contains(&RepoCiIssueType::Security));
    }
}

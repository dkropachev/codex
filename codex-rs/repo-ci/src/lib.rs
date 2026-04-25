use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

const MANIFEST_VERSION: u32 = 1;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    Fast,
    Full,
}

impl RunMode {
    fn as_str(self) -> &'static str {
        match self {
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
    pub automation: AutomationMode,
    pub local_test_time_budget_sec: u64,
    pub learned_at_unix_sec: u64,
    pub learning_sources: Vec<SourceHash>,
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

#[derive(Debug, Clone)]
pub struct LearnOptions {
    pub automation: AutomationMode,
    pub local_test_time_budget_sec: u64,
}

#[derive(Debug, Clone)]
pub struct LearnOutcome {
    pub paths: RepoCiPaths,
    pub manifest: RepoCiManifest,
    pub validation_exit_code: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusOutcome {
    pub paths: RepoCiPaths,
    pub manifest: Option<RepoCiManifest>,
    pub stale_sources: Vec<SourceHash>,
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
    let repo_root = repo_root_for_cwd(cwd)?;
    let repo_key = repo_key(&repo_root);
    let state_dir = codex_home.join("repo-ci").join(repo_key);
    Ok(RepoCiPaths {
        repo_root,
        manifest_path: state_dir.join("manifest.json"),
        runner_path: state_dir.join("run_ci.sh"),
        state_dir,
    })
}

pub fn learn(codex_home: &Path, cwd: &Path, options: LearnOptions) -> Result<LearnOutcome> {
    let paths = paths_for_repo(codex_home, cwd)?;
    fs::create_dir_all(&paths.state_dir)?;
    let learning_sources = collect_sources(&paths.repo_root)?;
    let (prepare_steps, fast_steps, full_steps) = infer_steps(&paths.repo_root)?;
    let mut manifest = RepoCiManifest {
        version: MANIFEST_VERSION,
        repo_key: repo_key(&paths.repo_root),
        repo_root: paths.repo_root.clone(),
        automation: options.automation,
        local_test_time_budget_sec: options.local_test_time_budget_sec,
        learned_at_unix_sec: unix_now(),
        learning_sources,
        prepare_steps,
        fast_steps,
        full_steps,
        validation: ValidationStatus::NotRun,
    };
    write_runner(&paths.runner_path, &manifest)?;
    write_manifest(&paths.manifest_path, &manifest)?;

    let prepare_status = run_runner(&paths, "prepare")?;
    let validation_status = if prepare_status.success() {
        run_runner(&paths, "fast")?
    } else {
        prepare_status
    };
    let validation_exit_code = validation_status.code();
    manifest.validation = if validation_status.success() {
        ValidationStatus::Passed {
            validated_at_unix_sec: unix_now(),
        }
    } else {
        ValidationStatus::Failed {
            exit_code: validation_exit_code,
        }
    };
    write_manifest(&paths.manifest_path, &manifest)?;

    Ok(LearnOutcome {
        paths,
        manifest,
        validation_exit_code,
    })
}

pub fn prepare(codex_home: &Path, cwd: &Path) -> Result<std::process::ExitStatus> {
    let paths = paths_for_repo(codex_home, cwd)?;
    require_runner(&paths)?;
    run_runner(&paths, "prepare")
}

pub fn run(codex_home: &Path, cwd: &Path, mode: RunMode) -> Result<std::process::ExitStatus> {
    let paths = paths_for_repo(codex_home, cwd)?;
    require_runner(&paths)?;
    run_runner(&paths, mode.as_str())
}

pub fn status(codex_home: &Path, cwd: &Path) -> Result<StatusOutcome> {
    let paths = paths_for_repo(codex_home, cwd)?;
    let manifest = if paths.manifest_path.exists() {
        Some(read_manifest(&paths.manifest_path)?)
    } else {
        None
    };
    let mut stale_sources = Vec::new();
    if let Some(manifest) = &manifest {
        for source in &manifest.learning_sources {
            let current = hash_file(&paths.repo_root.join(&source.path));
            if current.as_deref() != Some(source.sha256.as_str()) {
                stale_sources.push(source.clone());
            }
        }
    }
    Ok(StatusOutcome {
        paths,
        manifest,
        stale_sources,
    })
}

pub fn watch_pr(cwd: &Path) -> Result<std::process::ExitStatus> {
    let auth_status = Command::new("gh").arg("auth").arg("status").status();
    match auth_status {
        Ok(status) if status.success() => {}
        Ok(status) => return Err(anyhow!("`gh auth status` failed with {status}")),
        Err(err) => return Err(anyhow!("failed to run `gh auth status`: {err}")),
    }

    let mut command = Command::new("gh");
    command
        .arg("pr")
        .arg("checks")
        .arg("--watch")
        .arg("--fail-fast")
        .current_dir(cwd);
    command.status().context("failed to run `gh pr checks`")
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

fn run_runner(paths: &RepoCiPaths, arg: &str) -> Result<std::process::ExitStatus> {
    Command::new("bash")
        .arg(&paths.runner_path)
        .arg(arg)
        .current_dir(&paths.repo_root)
        .status()
        .with_context(|| format!("failed to run {}", paths.runner_path.display()))
}

fn write_manifest(path: &Path, manifest: &RepoCiManifest) -> Result<()> {
    let data = serde_json::to_vec_pretty(manifest)?;
    fs::write(path, data).with_context(|| format!("failed to write {}", path.display()))
}

fn read_manifest(path: &Path) -> Result<RepoCiManifest> {
    let data = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_slice(&data).with_context(|| format!("failed to parse {}", path.display()))
}

fn collect_sources(repo_root: &Path) -> Result<Vec<SourceHash>> {
    let mut sources = Vec::new();
    for (relative, kind) in source_candidates(repo_root)? {
        if let Some(sha256) = hash_file(&repo_root.join(&relative)) {
            sources.push(SourceHash {
                path: relative,
                sha256,
                kind,
            });
        }
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
        (PathBuf::from("package.json"), SourceKind::BuildManifest),
        (PathBuf::from("package-lock.json"), SourceKind::Lockfile),
        (PathBuf::from("pnpm-lock.yaml"), SourceKind::Lockfile),
        (PathBuf::from("yarn.lock"), SourceKind::Lockfile),
        (PathBuf::from("pyproject.toml"), SourceKind::BuildManifest),
        (PathBuf::from("requirements.txt"), SourceKind::BuildManifest),
        (PathBuf::from("uv.lock"), SourceKind::Lockfile),
        (PathBuf::from("tox.ini"), SourceKind::Tooling),
        (PathBuf::from("pytest.ini"), SourceKind::Tooling),
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
    let mut prepare = Vec::new();
    let mut fast = Vec::new();
    let mut full = Vec::new();

    if has_file(repo_root, "justfile") || has_file(repo_root, "Justfile") {
        let justfile =
            read_optional(repo_root, "justfile")?.or(read_optional(repo_root, "Justfile")?);
        if let Some(justfile) = justfile {
            add_just_steps(&justfile, &mut prepare, &mut fast, &mut full);
        }
    }

    if has_file(repo_root, "Cargo.toml") && fast.is_empty() {
        prepare.push(step("cargo-fetch", "cargo fetch", StepPhase::Prepare));
        fast.push(step(
            "cargo-fmt",
            "cargo fmt --all -- --check",
            StepPhase::Lint,
        ));
        fast.push(step(
            "cargo-clippy",
            "cargo clippy --workspace --all-targets",
            StepPhase::Lint,
        ));
        fast.push(step(
            "cargo-test",
            "cargo test --workspace",
            StepPhase::Test,
        ));
        full.extend(fast.clone());
    }

    if has_file(repo_root, "package.json") {
        add_node_steps(repo_root, &mut prepare, &mut fast, &mut full);
    }

    if has_file(repo_root, "pyproject.toml")
        || has_file(repo_root, "requirements.txt")
        || has_file(repo_root, "uv.lock")
    {
        add_python_steps(repo_root, &mut prepare, &mut fast, &mut full);
    }

    if fast.is_empty() {
        fast.push(step("git-diff-check", "git diff --check", StepPhase::Lint));
        full.extend(fast.clone());
    }

    Ok((prepare, fast, full))
}

fn add_just_steps(
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

fn add_node_steps(
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

fn add_python_steps(
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

fn justfile_has_recipe(justfile: &str, recipe: &str) -> bool {
    let prefix = format!("{recipe}:");
    justfile
        .lines()
        .any(|line| line.starts_with(&prefix) || line.starts_with(&format!("@{prefix}")))
}

fn step(id: &str, command: &str, phase: StepPhase) -> RepoCiStep {
    RepoCiStep {
        id: id.to_string(),
        command: command.to_string(),
        phase,
    }
}

fn read_optional(repo_root: &Path, relative: &str) -> Result<Option<String>> {
    let path = repo_root.join(relative);
    if path.exists() {
        fs::read_to_string(&path)
            .map(Some)
            .with_context(|| format!("failed to read {}", path.display()))
    } else {
        Ok(None)
    }
}

fn has_file(repo_root: &Path, relative: &str) -> bool {
    repo_root.join(relative).is_file()
}

fn hash_file(path: &Path) -> Option<String> {
    let data = fs::read(path).ok()?;
    let mut hasher = Sha256::new();
    hasher.update(data);
    Some(format!("{:x}", hasher.finalize()))
}

fn repo_key(repo_root: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(repo_root.to_string_lossy().as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    hash[..16].to_string()
}

fn write_runner(path: &Path, manifest: &RepoCiManifest) -> Result<()> {
    let mut script =
        String::from("#!/usr/bin/env bash\nset -euo pipefail\n\nmode=\"${1:-fast}\"\nrepo_root=");
    script.push_str(&shell_quote(&manifest.repo_root.to_string_lossy()));
    script.push_str("\ncd \"$repo_root\"\n\nrun_step() {\n  local id=\"$1\"\n  shift\n  echo \"==> ${id}\"\n  \"$@\"\n}\n\nprepare() {\n");
    for step in &manifest.prepare_steps {
        push_script_step(&mut script, step);
    }
    script.push_str("}\n\nfast() {\n  prepare\n");
    for step in &manifest.fast_steps {
        push_script_step(&mut script, step);
    }
    script.push_str("}\n\nfull() {\n  prepare\n");
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
            repo_key: repo_key(&repo),
            automation: AutomationMode::LocalAndRemote,
            local_test_time_budget_sec: 300,
            learned_at_unix_sec: 1,
            learning_sources: collect_sources(&repo).expect("sources"),
            prepare_steps: vec![],
            fast_steps: vec![],
            full_steps: vec![],
            validation: ValidationStatus::NotRun,
        };
        write_manifest(&paths.manifest_path, &manifest).expect("write manifest");

        fs::write(repo.join("Cargo.toml"), "[package]\nname = \"y\"\n").expect("update cargo");

        let status = status(&codex_home, &repo).expect("status");
        assert_eq!(status.stale_sources, manifest.learning_sources);
    }
}

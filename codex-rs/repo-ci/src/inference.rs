use anyhow::Result;
use std::collections::HashSet;
use std::path::Path;

use crate::RepoCiStep;
use crate::StepPhase;
use crate::add_just_steps;
use crate::add_node_steps;
use crate::add_python_steps;
use crate::has_file;
use crate::read_optional;
use crate::step;

pub(crate) fn infer_steps(
    repo_root: &Path,
) -> Result<(Vec<RepoCiStep>, Vec<RepoCiStep>, Vec<RepoCiStep>)> {
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

    add_make_steps(repo_root, &mut prepare, &mut fast, &mut full)?;

    if has_file(repo_root, "package.json") {
        add_node_steps(repo_root, &mut prepare, &mut fast, &mut full);
    }

    if has_file(repo_root, "pyproject.toml")
        || has_file(repo_root, "requirements.txt")
        || has_file(repo_root, "uv.lock")
    {
        add_python_steps(repo_root, &mut prepare, &mut fast, &mut full);
    }

    add_sbt_steps(repo_root, &mut fast, &mut full)?;
    dedup_steps(&mut prepare);
    dedup_steps(&mut fast);
    dedup_steps(&mut full);

    if fast.is_empty() {
        fast.push(step("git-diff-check", "git diff --check", StepPhase::Lint));
        full.extend(fast.clone());
    }

    Ok((prepare, fast, full))
}

fn add_make_steps(
    repo_root: &Path,
    prepare: &mut Vec<RepoCiStep>,
    fast: &mut Vec<RepoCiStep>,
    full: &mut Vec<RepoCiStep>,
) -> Result<()> {
    let Some(makefile) = read_makefile(repo_root)? else {
        return Ok(());
    };
    let targets = parse_make_targets(&makefile);

    for recipe in ["setup", "prepare"] {
        if targets.contains(recipe) {
            prepare.push(step(
                &format!("make-{recipe}"),
                &format!("make {recipe}"),
                StepPhase::Prepare,
            ));
        }
    }

    for (target, phase) in [("lint", StepPhase::Lint), ("build", StepPhase::Build)] {
        if targets.contains(target) {
            let ci_step = step(&format!("make-{target}"), &format!("make {target}"), phase);
            fast.push(ci_step.clone());
            full.push(ci_step);
        }
    }

    if targets.contains("test-unit") {
        let ci_step = step("make-test-unit", "make test-unit", StepPhase::Test);
        fast.push(ci_step.clone());
        full.push(ci_step);
    } else if targets.contains("test") {
        let ci_step = step("make-test", "make test", StepPhase::Test);
        fast.push(ci_step.clone());
        full.push(ci_step);
    }

    for target in [
        "test-integration",
        "integration",
        "test-integration-scylla",
        "test-e2e",
        "e2e",
    ] {
        if targets.contains(target) {
            full.push(step(
                &format!("make-{target}"),
                &format!("make {target}"),
                StepPhase::Test,
            ));
        }
    }

    Ok(())
}

fn add_sbt_steps(
    repo_root: &Path,
    fast: &mut Vec<RepoCiStep>,
    full: &mut Vec<RepoCiStep>,
) -> Result<()> {
    if !has_file(repo_root, "build.sbt") || !fast.is_empty() {
        return Ok(());
    }

    let plugins = read_optional(repo_root, "project/plugins.sbt")?;
    if has_file(repo_root, ".scalafmt.conf")
        || plugins
            .as_deref()
            .is_some_and(|value| value.contains("scalafmt"))
    {
        let ci_step = step(
            "sbt-scalafmt-check",
            "sbt scalafmtCheckAll",
            StepPhase::Lint,
        );
        fast.push(ci_step.clone());
        full.push(ci_step);
    }

    fast.push(step("sbt-compile", "sbt compile", StepPhase::Build));
    fast.push(step("sbt-test", "sbt test", StepPhase::Test));
    full.extend(fast.clone());
    Ok(())
}

fn read_makefile(repo_root: &Path) -> Result<Option<String>> {
    for relative in ["Makefile", "makefile", "GNUmakefile"] {
        if let Some(contents) = read_optional(repo_root, relative)? {
            return Ok(Some(contents));
        }
    }
    Ok(None)
}

fn parse_make_targets(makefile: &str) -> HashSet<&str> {
    let mut targets = HashSet::new();
    for line in makefile.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || line.starts_with('\t') {
            continue;
        }
        let Some((candidate, _)) = trimmed.split_once(':') else {
            continue;
        };
        if candidate.contains('=')
            || candidate.contains('%')
            || candidate.contains('$')
            || candidate.starts_with('.')
        {
            continue;
        }
        for target in candidate.split_whitespace() {
            if is_make_target_name(target) {
                targets.insert(target);
            }
        }
    }
    targets
}

fn is_make_target_name(candidate: &str) -> bool {
    !candidate.is_empty()
        && candidate
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
}

fn dedup_steps(steps: &mut Vec<RepoCiStep>) {
    let mut seen = HashSet::new();
    steps.retain(|step| seen.insert(step.id.clone()));
}

use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use crate::WorkflowPackage;
use crate::manifest::read_bounded_utf8;

use super::ValidationFinding;

const MAX_COVERAGE_TEST_FILES: usize = 256;
const MAX_COVERAGE_TEST_ENTRIES: usize = 1_024;
const MAX_COVERAGE_TEST_BYTES: usize = 1024 * 1024;
const MAX_COVERAGE_TEST_DEPTH: usize = 32;
pub(super) const MAX_GITIGNORE_BYTES: u64 = 64 * 1024;

pub(super) fn validate_coverage(
    package: &WorkflowPackage,
    findings: &mut BTreeSet<ValidationFinding>,
) {
    let coverage = &package.manifest.validation.coverage;
    let required = [
        ("positive", coverage.positive),
        ("load", coverage.load),
        ("autocomplete", coverage.autocomplete),
        ("negative", coverage.negative),
    ];
    let scan = coverage_markers(&package.root.join("src/tests"));
    if scan.exceeded_limit {
        findings.insert(ValidationFinding::new(
            "coverage",
            "test coverage scan exceeded its file, byte, entry, or depth limit",
        ));
    }
    let markers = scan.markers;
    for (name, enabled) in required {
        if !enabled {
            findings.insert(ValidationFinding::new(
                "coverage",
                format!("validation.coverage.{name} must be true"),
            ));
        }
        if !markers.contains(name) {
            findings.insert(ValidationFinding::new(
                "coverage",
                format!("tests are missing `workflow-covers: {name}` coverage"),
            ));
        }
    }
    if coverage.recovery && !markers.contains("recovery") {
        findings.insert(ValidationFinding::new(
            "coverage",
            "tests are missing `workflow-covers: recovery` coverage",
        ));
    }
}

#[derive(Default)]
struct CoverageScan {
    markers: BTreeSet<String>,
    files: usize,
    entries: usize,
    bytes: usize,
    exceeded_limit: bool,
}

fn coverage_markers(root: &Path) -> CoverageScan {
    let mut scan = CoverageScan::default();
    scan_coverage_markers(root, /*depth*/ 0, &mut scan);
    scan
}

fn scan_coverage_markers(root: &Path, depth: usize, scan: &mut CoverageScan) {
    if scan.exceeded_limit {
        return;
    }
    if depth > MAX_COVERAGE_TEST_DEPTH {
        scan.exceeded_limit = true;
        return;
    }
    let Ok(read_dir) = fs::read_dir(root) else {
        return;
    };
    let mut entries = Vec::new();
    for entry in read_dir.flatten() {
        scan.entries += 1;
        if scan.entries > MAX_COVERAGE_TEST_ENTRIES {
            scan.exceeded_limit = true;
            return;
        }
        entries.push(entry);
    }
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            scan_coverage_markers(&path, depth + 1, scan);
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        scan.files += 1;
        if scan.files > MAX_COVERAGE_TEST_FILES {
            scan.exceeded_limit = true;
            return;
        }
        let remaining = MAX_COVERAGE_TEST_BYTES.saturating_sub(scan.bytes);
        let Ok(file) = fs::File::open(path) else {
            continue;
        };
        let mut contents = String::new();
        if file
            .take(remaining.saturating_add(1) as u64)
            .read_to_string(&mut contents)
            .is_err()
        {
            continue;
        }
        if contents.len() > remaining {
            scan.exceeded_limit = true;
            return;
        }
        scan.bytes += contents.len();
        for line in contents.lines() {
            let Some((_, values)) = line.split_once("workflow-covers:") else {
                continue;
            };
            scan.markers.extend(
                values
                    .split(|ch: char| ch.is_whitespace() || ch == ',')
                    .filter(|value| !value.is_empty())
                    .map(ToString::to_string),
            );
        }
    }
}

pub(super) fn validate_commands(
    package: &WorkflowPackage,
    findings: &mut BTreeSet<ValidationFinding>,
) {
    for (index, command) in package.manifest.validation.commands.iter().enumerate() {
        if command.program.trim().is_empty() {
            findings.insert(ValidationFinding::new(
                "command",
                format!("validation command {index} has an empty program"),
            ));
            continue;
        }
        let mut process = Command::new(&command.program);
        process.args(&command.args).current_dir(&package.root);
        match crate::runner::run_bounded_command(
            process,
            Duration::from_secs(/*secs*/ 60),
            64 * 1024,
            Some(&AtomicBool::new(false)),
        ) {
            Ok((status, _, _, _)) if status.success() => {}
            Ok((status, _, _, _)) => {
                let status = status
                    .code()
                    .map_or_else(|| "terminated".to_string(), |code| code.to_string());
                findings.insert(ValidationFinding::new(
                    "command",
                    format!(
                        "validation command {index} `{}` with args {:?} failed with exit status {status}",
                        command.program,
                        command.args,
                    ),
                ));
            }
            Err(err) => {
                let outcome = if format!("{err:#}").contains("timed out") {
                    "timed out after 60000 ms"
                } else {
                    "could not start or complete"
                };
                findings.insert(ValidationFinding::new(
                    "command",
                    format!(
                        "validation command {index} `{}` with args {:?} {outcome}",
                        command.program, command.args,
                    ),
                ));
            }
        }
    }
}

pub(super) fn validate_gitignore(root: &Path, findings: &mut BTreeSet<ValidationFinding>) {
    let path = root.join(".gitignore");
    let contents = match read_bounded_utf8(&path, MAX_GITIGNORE_BYTES) {
        Ok(contents) => contents,
        Err(err) => {
            findings.insert(ValidationFinding::new(
                "gitignore",
                format!("failed to read .gitignore: {err:#}"),
            ));
            return;
        }
    };
    for required in ["node_modules/", "artifacts/", "state/*", "!state/.gitkeep"] {
        if !contents.lines().any(|line| line.trim() == required) {
            findings.insert(ValidationFinding::new(
                "gitignore",
                format!(".gitignore is missing `{required}`"),
            ));
        }
    }
}

pub(super) fn validate_git_layout(root: &Path, findings: &mut BTreeSet<ValidationFinding>) {
    if !root.join(".git").is_dir() {
        return;
    }
    let output = match Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "--", "node_modules", "artifacts", "state"])
        .output()
    {
        Ok(output) => output,
        Err(err) => {
            findings.insert(ValidationFinding::new(
                "layout",
                format!("failed to inspect workflow git repository: {err}"),
            ));
            return;
        }
    };
    if !output.status.success() {
        findings.insert(ValidationFinding::new(
            "layout",
            format!(
                "failed to inspect workflow git repository: git ls-files exited with {}",
                output.status
            ),
        ));
        return;
    }
    for path in String::from_utf8_lossy(&output.stdout).lines() {
        if path != "state/.gitkeep" {
            findings.insert(ValidationFinding::new(
                "layout",
                format!("generated or runtime file `{path}` must not be tracked"),
            ));
        }
    }
}

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RepoSnapshot {
    pub repo_identity: String,
    pub repo_family: String,
    pub remote_hash: Option<String>,
    pub path_hash: String,
    pub work_size: WorkSize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkSize {
    pub tracked_files: usize,
    pub changed_files: usize,
    pub changed_lines: usize,
    pub touched_modules: usize,
    pub language_mix: BTreeMap<String, usize>,
    pub shared_api_changed: bool,
    pub ui_changed: bool,
    pub tests_changed: bool,
    pub work_size_units: u32,
    pub repo_tshirt_bucket: String,
}

pub(crate) fn repo_snapshot(cwd: &Path) -> RepoSnapshot {
    let remote = git_output(cwd, ["config", "--get", "remote.origin.url"]);
    let remote_hash = remote.as_deref().map(hash_string);
    let path_hash = hash_string(&canonical_path(cwd));
    let work_size = work_size(cwd);
    let language_key = work_size
        .language_mix
        .keys()
        .take(4)
        .cloned()
        .collect::<Vec<_>>()
        .join("+");
    let size_bucket = match work_size.work_size_units {
        0..=5 => "xs",
        6..=20 => "sm",
        21..=80 => "md",
        81..=200 => "lg",
        _ => "xl",
    };
    let framework_hints = framework_hints(cwd).join("+");
    let shape = format!(
        "langs={language_key};size={size_bucket};bucket={};api={};ui={};tests={};frameworks={framework_hints}",
        work_size.repo_tshirt_bucket,
        work_size.shared_api_changed,
        work_size.ui_changed,
        work_size.tests_changed
    );
    let repo_identity = remote_hash.clone().unwrap_or_else(|| path_hash.clone());
    RepoSnapshot {
        repo_identity,
        repo_family: hash_string(&shape),
        remote_hash,
        path_hash,
        work_size,
    }
}

fn work_size(cwd: &Path) -> WorkSize {
    let diff = git_output(cwd, ["diff", "--numstat", "HEAD", "--"]).unwrap_or_default();
    let tracked_files = git_output(cwd, ["ls-files"])
        .map(|output| {
            output
                .lines()
                .filter(|line| !line.trim().is_empty())
                .count()
        })
        .unwrap_or(0);
    let mut changed_files = 0usize;
    let mut changed_lines = 0usize;
    let mut modules = BTreeMap::new();
    let mut language_mix = BTreeMap::new();
    let mut shared_api_changed = false;
    let mut ui_changed = false;
    let mut tests_changed = false;

    for line in diff.lines() {
        let mut parts = line.split('\t');
        let added = parts.next().and_then(parse_numstat_count).unwrap_or(0);
        let removed = parts.next().and_then(parse_numstat_count).unwrap_or(0);
        let Some(path) = parts.next() else {
            continue;
        };
        changed_files += 1;
        changed_lines += added + removed;
        if let Some(module) = path.split('/').next()
            && !module.is_empty()
        {
            modules.insert(module.to_string(), ());
        }
        *language_mix
            .entry(language_for_path(path).to_string())
            .or_insert(0) += 1;
        shared_api_changed |= is_shared_api_path(path);
        ui_changed |= is_ui_path(path);
        tests_changed |= is_test_path(path);
    }

    if changed_files == 0 {
        let status = git_output(cwd, ["status", "--short"]).unwrap_or_default();
        for line in status.lines() {
            let path = line.get(3..).unwrap_or(line).trim();
            if path.is_empty() {
                continue;
            }
            changed_files += 1;
            changed_lines += 1;
            if let Some(module) = path.split('/').next()
                && !module.is_empty()
            {
                modules.insert(module.to_string(), ());
            }
            *language_mix
                .entry(language_for_path(path).to_string())
                .or_insert(0) += 1;
            shared_api_changed |= is_shared_api_path(path);
            ui_changed |= is_ui_path(path);
            tests_changed |= is_test_path(path);
        }
    }

    let touched_modules = modules.len();
    let mut units = changed_files as u32
        + (changed_lines as u32 / 25)
        + touched_modules as u32
        + u32::from(shared_api_changed) * 8
        + u32::from(ui_changed) * 3
        + u32::from(tests_changed) * 2;
    if units == 0 {
        units = 1;
    }

    WorkSize {
        tracked_files,
        changed_files,
        changed_lines,
        touched_modules,
        language_mix,
        shared_api_changed,
        ui_changed,
        tests_changed,
        work_size_units: units,
        repo_tshirt_bucket: repo_tshirt_bucket(
            tracked_files,
            changed_files,
            changed_lines,
            units,
            shared_api_changed,
            ui_changed,
            tests_changed,
        )
        .to_string(),
    }
}

fn repo_tshirt_bucket(
    tracked_files: usize,
    changed_files: usize,
    changed_lines: usize,
    work_size_units: u32,
    shared_api_changed: bool,
    ui_changed: bool,
    tests_changed: bool,
) -> &'static str {
    let mut points = match tracked_files {
        0..=50 => 0,
        51..=250 => 1,
        251..=1000 => 2,
        1001..=5000 => 3,
        _ => 4,
    };
    points += match changed_files {
        0..=2 => 0,
        3..=8 => 1,
        9..=25 => 2,
        26..=80 => 3,
        _ => 4,
    };
    points += match changed_lines {
        0..=50 => 0,
        51..=250 => 1,
        251..=1000 => 2,
        1001..=3000 => 3,
        _ => 4,
    };
    points += match work_size_units {
        0..=10 => 0,
        11..=40 => 1,
        41..=120 => 2,
        121..=300 => 3,
        _ => 4,
    };
    points += u32::from(shared_api_changed) * 2;
    points += u32::from(ui_changed);
    points += u32::from(tests_changed);

    match points {
        0..=1 => "XS",
        2..=4 => "S",
        5..=8 => "M",
        9..=12 => "L",
        _ => "XL",
    }
}

fn framework_hints(cwd: &Path) -> Vec<&'static str> {
    let mut hints = Vec::new();
    if cwd.join("Cargo.toml").exists() {
        hints.push("rust");
    }
    if cwd.join("package.json").exists() {
        hints.push("node");
    }
    if cwd.join("pyproject.toml").exists() {
        hints.push("python");
    }
    if cwd.join("go.mod").exists() {
        hints.push("go");
    }
    hints
}

fn git_output<const N: usize>(cwd: &Path, args: [&str; N]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn canonical_path(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

fn hash_string(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn parse_numstat_count(value: &str) -> Option<usize> {
    if value == "-" {
        return Some(1);
    }
    value.parse().ok()
}

fn language_for_path(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
    {
        Some("rs") => "rust",
        Some("ts" | "tsx" | "js" | "jsx") => "typescript",
        Some("py") => "python",
        Some("go") => "go",
        Some("java" | "kt") => "jvm",
        Some("swift") => "swift",
        Some("md" | "mdx") => "docs",
        Some("json" | "yaml" | "yml" | "toml") => "config",
        _ => "other",
    }
}

fn is_shared_api_path(path: &str) -> bool {
    path.contains("protocol")
        || path.contains("schema")
        || path.ends_with("Cargo.toml")
        || path.ends_with("Cargo.lock")
        || path.contains("/api/")
}

fn is_ui_path(path: &str) -> bool {
    path.contains("/tui/")
        || path.contains("/ui/")
        || path.contains("/frontend/")
        || path.ends_with(".tsx")
        || path.ends_with(".jsx")
        || path.ends_with(".css")
}

fn is_test_path(path: &str) -> bool {
    path.contains("/tests/")
        || path.contains("_test.")
        || path.contains("_tests.")
        || path.ends_with(".snap")
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn language_and_shape_helpers_classify_paths() {
        assert_eq!(language_for_path("src/lib.rs"), "rust");
        assert_eq!(language_for_path("web/App.tsx"), "typescript");
        assert!(is_shared_api_path("app-server-protocol/src/protocol/v2.rs"));
        assert!(is_ui_path("codex-rs/tui/src/app.rs"));
        assert!(is_test_path("src/tests/pipeline.rs"));
        assert_eq!(repo_tshirt_bucket(200, 10, 300, 40, true, false, true), "L");
    }
}

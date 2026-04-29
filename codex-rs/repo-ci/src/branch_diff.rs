use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

/// Summary of the whole branch diff used by repo-ci targeted review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchDiffSnapshot {
    /// The default branch ref selected as the branch review base.
    pub base_ref: Option<String>,
    /// The merge-base between HEAD and `base_ref`.
    pub merge_base: Option<String>,
    /// Paths changed across the branch diff plus untracked files.
    pub changed_paths: Vec<String>,
    /// Human-readable diff summary for the branch review prompt.
    pub diff_summary: String,
}

impl BranchDiffSnapshot {
    /// Capture changed paths and a diff summary for the current branch.
    pub fn capture(cwd: &Path) -> Self {
        let base_ref = default_branch_ref(cwd);
        let merge_base = base_ref
            .as_deref()
            .and_then(|base_ref| git_stdout(cwd, &["merge-base", "HEAD", base_ref]));

        let changed_paths = branch_changed_paths(cwd, merge_base.as_deref());
        let diff_summary = branch_diff_summary(cwd, merge_base.as_deref(), &changed_paths);

        Self {
            base_ref,
            merge_base,
            changed_paths,
            diff_summary,
        }
    }

    /// Describe which base ref and merge-base were used for this snapshot.
    pub fn scope_description(&self) -> String {
        match (&self.base_ref, &self.merge_base) {
            (Some(base_ref), Some(merge_base)) => {
                format!("Whole branch diff against `{base_ref}` using merge base `{merge_base}`.")
            }
            (Some(base_ref), None) => format!(
                "Whole branch diff requested against `{base_ref}`, but no merge base was found; using current uncommitted changes only."
            ),
            (None, _) => {
                "No default branch ref was detected; using current uncommitted changes only."
                    .to_string()
            }
        }
    }
}

fn branch_changed_paths(cwd: &Path, merge_base: Option<&str>) -> Vec<String> {
    let mut paths = BTreeSet::new();
    if let Some(merge_base) = merge_base {
        extend_lines(
            &mut paths,
            git_stdout(cwd, &["diff", "--name-only", merge_base]).as_deref(),
        );
    } else {
        extend_lines(
            &mut paths,
            git_stdout(cwd, &["diff", "--name-only"]).as_deref(),
        );
        extend_lines(
            &mut paths,
            git_stdout(cwd, &["diff", "--cached", "--name-only"]).as_deref(),
        );
    }
    extend_lines(
        &mut paths,
        git_stdout(cwd, &["ls-files", "--others", "--exclude-standard"]).as_deref(),
    );
    paths.into_iter().collect()
}

fn branch_diff_summary(cwd: &Path, merge_base: Option<&str>, changed_paths: &[String]) -> String {
    let mut parts = Vec::new();
    if let Some(merge_base) = merge_base {
        if let Some(summary) = git_stdout(cwd, &["diff", "--stat", merge_base])
            && !summary.trim().is_empty()
        {
            parts.push(summary);
        }
    } else {
        for args in [&["diff", "--stat"][..], &["diff", "--cached", "--stat"][..]] {
            if let Some(summary) = git_stdout(cwd, args)
                && !summary.trim().is_empty()
            {
                parts.push(summary);
            }
        }
    }

    let untracked = git_stdout(cwd, &["ls-files", "--others", "--exclude-standard"])
        .map(|stdout| {
            stdout
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| format!("  {line}"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !untracked.is_empty() {
        parts.push(format!("Untracked files:\n{}", untracked.join("\n")));
    }

    if parts.is_empty() && !changed_paths.is_empty() {
        format!("Changed paths:\n{}", changed_paths.join("\n"))
    } else {
        parts.join("\n")
    }
}

fn default_branch_ref(cwd: &Path) -> Option<String> {
    git_stdout(
        cwd,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )
    .or_else(|| resolve_ref(cwd, "origin/main").map(|_| "origin/main".to_string()))
    .or_else(|| resolve_ref(cwd, "origin/master").map(|_| "origin/master".to_string()))
    .or_else(|| resolve_ref(cwd, "main").map(|_| "main".to_string()))
    .or_else(|| resolve_ref(cwd, "master").map(|_| "master".to_string()))
}

fn resolve_ref(cwd: &Path, git_ref: &str) -> Option<String> {
    git_stdout(cwd, &["rev-parse", "--verify", "--quiet", git_ref])
}

fn extend_lines(paths: &mut BTreeSet<String>, stdout: Option<&str>) {
    if let Some(stdout) = stdout {
        paths.extend(
            stdout
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string),
        );
    }
}

fn git_stdout(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!stdout.is_empty()).then_some(stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::fs;
    use tempfile::TempDir;

    fn run_git(repo: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(repo)
            .status()
            .expect("git command");
        assert!(status.success(), "git {args:?} failed");
    }

    fn write(repo: &Path, path: &str, contents: &str) {
        let path = repo.join(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, contents).expect("write file");
    }

    fn commit_all(repo: &Path, message: &str) {
        run_git(repo, &["add", "--all"]);
        run_git(repo, &["commit", "-m", message]);
    }

    fn test_repo() -> TempDir {
        let temp = TempDir::new().expect("tempdir");
        run_git(temp.path(), &["init", "-b", "main"]);
        run_git(temp.path(), &["config", "user.email", "test@example.com"]);
        run_git(temp.path(), &["config", "user.name", "Test User"]);
        write(temp.path(), "src/lib.rs", "pub fn base() {}\n");
        commit_all(temp.path(), "base");
        temp
    }

    #[test]
    fn branch_snapshot_includes_committed_branch_changes() {
        let temp = test_repo();
        run_git(temp.path(), &["checkout", "-b", "feature"]);
        write(
            temp.path(),
            "src/lib.rs",
            "pub fn base() {}\npub fn feature() {}\n",
        );
        commit_all(temp.path(), "feature");

        let snapshot = BranchDiffSnapshot::capture(temp.path());

        assert_eq!(snapshot.base_ref, Some("main".to_string()));
        assert!(snapshot.merge_base.is_some());
        assert_eq!(snapshot.changed_paths, vec!["src/lib.rs".to_string()]);
        assert!(snapshot.diff_summary.contains("src/lib.rs"));
    }

    #[test]
    fn branch_snapshot_adds_untracked_paths_to_branch_diff() {
        let temp = test_repo();
        run_git(temp.path(), &["checkout", "-b", "feature"]);
        write(
            temp.path(),
            "src/lib.rs",
            "pub fn base() {}\npub fn feature() {}\n",
        );
        commit_all(temp.path(), "feature");
        write(temp.path(), "src/new.rs", "pub fn untracked() {}\n");

        let snapshot = BranchDiffSnapshot::capture(temp.path());

        assert_eq!(
            snapshot.changed_paths,
            vec!["src/lib.rs".to_string(), "src/new.rs".to_string()]
        );
        assert!(snapshot.diff_summary.contains("Untracked files"));
        assert!(snapshot.diff_summary.contains("src/new.rs"));
    }
}

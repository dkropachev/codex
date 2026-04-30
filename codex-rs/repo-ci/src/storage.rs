use sha2::Digest;
use sha2::Sha256;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

pub(crate) fn artifact_state_dir(codex_home: &Path, repo_root: &Path) -> PathBuf {
    let repo_key = repo_key(repo_root);
    let first = &repo_key[..2];
    let second = &repo_key[2..4];
    codex_home
        .join("repo-ci")
        .join("artifacts")
        .join(first)
        .join(second)
        .join(repo_key)
}

pub(crate) fn repo_key(repo_root: &Path) -> String {
    let remote_repo = git_output(repo_root, &["remote", "get-url", "origin"])
        .or_else(|| {
            let remotes = git_output(repo_root, &["remote"])?;
            remotes
                .lines()
                .map(str::trim)
                .find(|remote| !remote.is_empty())
                .and_then(|remote| git_output(repo_root, &["remote", "get-url", remote]))
        })
        .map(|remote| normalize_remote_repo(&remote));
    let commit_hash = git_output(repo_root, &["rev-parse", "HEAD"]);
    let identity = match (remote_repo, commit_hash) {
        (Some(remote_repo), Some(commit_hash)) => {
            format!("remote:{remote_repo}\ncommit:{commit_hash}")
        }
        (None, Some(commit_hash)) => {
            format!(
                "local:{}\ncommit:{commit_hash}",
                repo_root.to_string_lossy()
            )
        }
        (Some(_), None) | (None, None) => format!("local:{}", repo_root.to_string_lossy()),
    };
    let mut hasher = Sha256::new();
    hasher.update(identity.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn git_output(repo_root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn normalize_remote_repo(remote_url: &str) -> String {
    let remote = remote_url
        .trim()
        .trim_end_matches('/')
        .trim_end_matches(".git");
    let parsed = (|| -> Option<(String, String)> {
        if let Some(rest) = remote
            .strip_prefix("https://")
            .or_else(|| remote.strip_prefix("http://"))
            .or_else(|| remote.strip_prefix("ssh://"))
            .or_else(|| remote.strip_prefix("git://"))
        {
            let rest = rest.split(['?', '#']).next().unwrap_or(rest);
            let (host, path) = rest.split_once('/')?;
            let host = host.rsplit('@').next()?.to_string();
            return Some((host, trim_remote_path(path)));
        }

        if remote.contains("://") {
            return None;
        }

        let (host, path) = remote.split_once(':')?;
        if host.contains('/') {
            return None;
        }
        let host = host.rsplit('@').next()?.to_string();
        Some((host, trim_remote_path(path)))
    })();
    parsed
        .map(|(host, path)| {
            let host = host.to_ascii_lowercase();
            let path = if host == "github.com" {
                path.to_ascii_lowercase()
            } else {
                path
            };
            format!("{host}/{path}")
        })
        .unwrap_or_else(|| remote.to_string())
}

fn trim_remote_path(path: &str) -> String {
    path.trim_matches('/').trim_end_matches(".git").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn normalizes_common_remote_url_forms() {
        assert_eq!(
            normalize_remote_repo("git@github.com:OpenAI/codex.git"),
            "github.com/openai/codex"
        );
        assert_eq!(
            normalize_remote_repo("https://github.com/openai/codex.git"),
            "github.com/openai/codex"
        );
        assert_eq!(
            normalize_remote_repo("ssh://git@github.com/openai/codex.git"),
            "github.com/openai/codex"
        );
    }

    #[test]
    fn artifact_location_is_sharded_under_repo_ci_artifacts() {
        let codex_home = Path::new("/tmp/codex-home");
        let repo_root = Path::new("/tmp/repo");
        let repo_key = repo_key(repo_root);
        let expected = codex_home
            .join("repo-ci")
            .join("artifacts")
            .join(&repo_key[..2])
            .join(&repo_key[2..4])
            .join(&repo_key);

        assert_eq!(artifact_state_dir(codex_home, repo_root), expected);
    }

    #[test]
    fn same_remote_and_commit_share_artifact_location() {
        let temp = TempDir::new().expect("tempdir");
        let codex_home = temp.path().join("codex-home");
        let origin = temp.path().join("origin.git");
        let source = temp.path().join("source");
        let first = temp.path().join("first");
        let second = temp.path().join("second");

        git(
            temp.path(),
            &["init", "--bare", origin.to_str().expect("utf8 path")],
        );
        fs::create_dir(&source).expect("create source");
        git(&source, &["init"]);
        git(&source, &["config", "user.email", "repo-ci@example.com"]);
        git(&source, &["config", "user.name", "Repo CI"]);
        fs::write(source.join("README.md"), "hello\n").expect("write readme");
        git(&source, &["add", "README.md"]);
        git(&source, &["commit", "-m", "initial"]);
        git(
            &source,
            &[
                "remote",
                "add",
                "origin",
                origin.to_str().expect("utf8 path"),
            ],
        );
        git(&source, &["push", "origin", "HEAD:main"]);
        git(&origin, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        git(
            temp.path(),
            &[
                "clone",
                origin.to_str().expect("utf8 path"),
                first.to_str().expect("utf8 path"),
            ],
        );
        git(
            temp.path(),
            &[
                "clone",
                origin.to_str().expect("utf8 path"),
                second.to_str().expect("utf8 path"),
            ],
        );

        let first_location = artifact_state_dir(&codex_home, &first);
        let second_location = artifact_state_dir(&codex_home, &second);

        assert_eq!(first_location, second_location);
    }

    #[test]
    fn commit_hash_changes_artifact_location() {
        let temp = TempDir::new().expect("tempdir");
        let codex_home = temp.path().join("codex-home");
        let repo = temp.path().join("repo");
        fs::create_dir(&repo).expect("create repo");
        git(&repo, &["init"]);
        git(&repo, &["config", "user.email", "repo-ci@example.com"]);
        git(&repo, &["config", "user.name", "Repo CI"]);
        git(
            &repo,
            &["remote", "add", "origin", "git@github.com:openai/codex.git"],
        );
        fs::write(repo.join("README.md"), "one\n").expect("write readme");
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "-m", "first"]);
        let first_location = artifact_state_dir(&codex_home, &repo);

        fs::write(repo.join("README.md"), "two\n").expect("update readme");
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "-m", "second"]);
        let second_location = artifact_state_dir(&codex_home, &repo);

        assert_ne!(first_location, second_location);
    }

    fn git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

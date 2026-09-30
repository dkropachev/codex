use super::*;
use pretty_assertions::assert_eq;
use std::ffi::OsStr;

#[test]
fn parses_existing_local_path_and_file_url() {
    let temp = tempfile::tempdir().expect("temp dir");
    let canonical = AbsolutePathBuf::from_absolute_path(temp.path())
        .and_then(|path| path.canonicalize())
        .expect("canonical temp dir");
    let local = WorkflowGitSource(WorkflowGitSourceKind::Local(canonical));

    assert_eq!(
        WorkflowGitSource::parse(&format!("  {}  ", temp.path().display())).expect("local source"),
        local
    );
    let file_url = Url::from_directory_path(temp.path()).expect("file URL");
    assert_eq!(
        WorkflowGitSource::parse(file_url.as_str()).expect("file source"),
        local
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let target = temp.path().join("target");
        let link = temp.path().join("link");
        std::fs::create_dir(&target).expect("create symlink target");
        symlink(&target, &link).expect("create local source symlink");
        let parsed = WorkflowGitSource::parse(link.to_str().expect("UTF-8 symlink path"))
            .expect("symlinked local source");
        let canonical = AbsolutePathBuf::from_absolute_path(&target)
            .and_then(|path| path.canonicalize())
            .expect("canonical symlink target");
        assert_eq!(
            parsed,
            WorkflowGitSource(WorkflowGitSourceKind::Local(canonical))
        );

        assert_invalid(&format!("/{}", temp.path().display()));
        assert_invalid(&format!("file:///{}", temp.path().display()));
        let relative = temp.path().strip_prefix("/").expect("absolute temp path");
        assert_invalid(&format!("file:///%2F{}", relative.display()));
    }
}

#[test]
fn parses_supported_remote_forms_for_command_args() {
    let cases = [
        (
            "https://github.com/openai/codex.git",
            WorkflowGitSourceKind::Https("https://github.com/openai/codex.git".into()),
        ),
        (
            "ssh://git@github.com/openai/codex.git",
            WorkflowGitSourceKind::Ssh("ssh://git@github.com/openai/codex.git".into()),
        ),
        (
            "git@github.com:openai/codex.git",
            WorkflowGitSourceKind::Ssh("git@github.com:openai/codex.git".into()),
        ),
        (
            "git@[::1]:openai/codex.git",
            WorkflowGitSourceKind::Ssh("git@[::1]:openai/codex.git".into()),
        ),
    ];
    for (input, kind) in cases {
        let parsed = WorkflowGitSource::parse(input).expect("remote source");
        assert_eq!(parsed, WorkflowGitSource(kind));
        assert_eq!(parsed.as_os_str(), OsStr::new(input));
    }
}

#[test]
fn rejects_unsafe_or_malformed_sources_without_disclosing_credentials() {
    for input in [
        "",
        "   ",
        "https://github.com/a\0b",
        "https://github.com/a\nb",
        "http://github.com/openai/codex",
        "ftp://github.com/openai/codex",
        "git://github.com/openai/codex",
        "https:///openai/codex",
        "HTTPS://github.com/openai/codex",
        "SSH://git@github.com/openai/codex",
        "https://github.com/",
        "https://user@github.com/openai/codex",
        "ssh://git:secret@github.com/openai/codex",
        "ssh://@github.com/openai/codex",
        "ssh://-oProxyCommand/openai/codex",
        "ssh://github..com/openai/codex",
        "ssh://git@foo.-bar.example/openai/codex",
        "ssh://git@999.999.999.999/openai/codex",
        "ssh://.git@github.com/openai/codex",
        "ssh://github.com/",
        "ssh://github.com/openai/codex?ref=main",
        "https://github.com/openai/codex#main",
        "file:///definitely/not/a/workflow/repository",
        "file://server/share/workflow",
        "file:////server/share/workflow",
        "file:///%5C%5Cserver%5Cshare%5Cworkflow",
        r"\\server\share\workflow",
        r"\\?\UNC\server\share\workflow",
        r"/\server\share\workflow",
        r"\/server/share/workflow",
        "//server/share/workflow",
        "ext::sh -c exploit",
        "hg::https://github.com/openai/codex",
        "_helper::https://github.com/openai/codex",
        "git@github.com:",
        "git:secret@github.com:openai/codex",
        "-oProxyCommand@github.com:openai/codex",
        "git@github..com:openai/codex",
        "git@foo.-bar.example:openai/codex",
        "git@999.999.999.999:openai/codex",
        ".git@github.com:openai/codex",
        "git@github.com:openai/codex?ref=main",
        "git@github.com:openai/codex#main",
        "C:/definitely/missing/workflow/repository",
        "C:relative",
        "C:",
    ] {
        assert_invalid(input);
    }
    let oversized = format!("https://example.com/{}.git", "x".repeat(MAX_SOURCE_LEN));
    assert_invalid(&oversized);
    assert_invalid("https://user:do-not-print-this@github.com/openai/codex");
}

#[test]
fn validates_network_paths_and_host_boundaries() {
    for path in [
        r"\\server\share",
        "//server/share",
        r"/\server\share",
        r"\/server/share",
    ] {
        assert!(is_network_path(path), "missed network path {path:?}");
        assert_eq!(
            canonical_file_path(Path::new(path))
                .expect_err("network file path should be rejected")
                .to_string(),
            INVALID_SOURCE
        );
    }
    for path in [r"C:\repo", r"\\?\C:\repo", "/tmp/repo"] {
        assert!(!is_network_path(path), "rejected local path {path:?}");
    }
    for source in [
        "ssh://git@127.0.0.1/openai/codex",
        "git@127.0.0.1:openai/codex",
        "ssh://git@[::1]/openai/codex",
        "git@foo-bar.example:openai/codex",
        "user.name+ci@github.com:openai/codex",
    ] {
        WorkflowGitSource::parse(source).expect("valid host or username");
    }
}

fn assert_invalid(source: &str) {
    let Err(error) = WorkflowGitSource::parse(source) else {
        panic!("accepted invalid source {source:?}");
    };
    assert_eq!(error.to_string(), INVALID_SOURCE);
}

#[cfg(windows)]
#[test]
fn parses_verbatim_local_path_without_preserving_the_device_prefix() {
    let temp = tempfile::tempdir().expect("temp dir");
    let source = format!(r"\\?\{}", temp.path().display());
    let parsed = WorkflowGitSource::parse(&source).expect("verbatim local source");
    let canonical = AbsolutePathBuf::from_absolute_path(temp.path())
        .and_then(|path| path.canonicalize())
        .expect("canonical temp dir");
    assert_eq!(
        parsed,
        WorkflowGitSource(WorkflowGitSourceKind::Local(canonical))
    );
}

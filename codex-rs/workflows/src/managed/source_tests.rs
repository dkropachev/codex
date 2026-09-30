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
        local.clone()
    );
    let file_url = Url::from_directory_path(temp.path()).expect("file URL");
    assert_eq!(
        WorkflowGitSource::parse(file_url.as_str()).expect("file source"),
        local
    );
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
        "https://github.com/",
        "https://user@github.com/openai/codex",
        "ssh://git:secret@github.com/openai/codex",
        "ssh://@github.com/openai/codex",
        "ssh://-oProxyCommand/openai/codex",
        "ssh://github..com/openai/codex",
        "ssh://github.com/",
        "ssh://github.com/openai/codex?ref=main",
        "https://github.com/openai/codex#main",
        "file:///definitely/not/a/workflow/repository",
        "file://server/share/workflow",
        r"\\server\share\workflow",
        r"\\?\UNC\server\share\workflow",
        "//server/share/workflow",
        "ext::sh -c exploit",
        "hg::https://github.com/openai/codex",
        "_helper::https://github.com/openai/codex",
        "git@github.com:",
        "git:secret@github.com:openai/codex",
        "-oProxyCommand@github.com:openai/codex",
        "git@github..com:openai/codex",
        "git@github.com:openai/codex?ref=main",
        "git@github.com:openai/codex#main",
        "C:/definitely/missing/workflow/repository",
    ] {
        assert!(
            WorkflowGitSource::parse(input).is_err(),
            "accepted {input:?}"
        );
    }
    assert!(WorkflowGitSource::parse(&"x".repeat(MAX_SOURCE_LEN + 1)).is_err());

    let secret = "do-not-print-this";
    let error = WorkflowGitSource::parse(&format!("https://user:{secret}@github.com/openai/codex"))
        .expect_err("credentials must be rejected")
        .to_string();
    assert!(!error.contains(secret));
}

#[cfg(windows)]
#[test]
fn parses_verbatim_local_path_without_preserving_the_device_prefix() {
    let temp = tempfile::tempdir().expect("temp dir");
    let source = format!(r"\\?\{}", temp.path().display());
    let parsed = WorkflowGitSource::parse(&source).expect("verbatim local source");
    assert!(!parsed.as_os_str().to_string_lossy().starts_with(r"\\?\"));
}

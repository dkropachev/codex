use pretty_assertions::assert_eq;

use super::source_for_host;

#[test]
fn omitted_cwd_uses_configured_host_directory() {
    let configured_cwd = tempfile::tempdir().expect("configured cwd");
    let source = source_for_host("./sources/review", /*cwd*/ None, configured_cwd.path())
        .expect("relative source");
    assert_eq!(
        source,
        configured_cwd
            .path()
            .join("./sources/review")
            .to_str()
            .expect("UTF-8 path")
    );
    assert_eq!(
        source_for_host(
            "https://example.com/review.git",
            /*cwd*/ None,
            configured_cwd.path(),
        )
        .expect("remote source"),
        "https://example.com/review.git"
    );
    assert!(source_for_host("./review", Some("relative"), configured_cwd.path()).is_err());
}

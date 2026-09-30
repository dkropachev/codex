use pretty_assertions::assert_eq;
use semver::Version;

use super::ResolvedWorkflowRelease;
use super::resolve_workflow_release;

const HEAD: &str = "1111111111111111111111111111111111111111";
const FIRST: &str = "2222222222222222222222222222222222222222";
const SECOND: &str = "3333333333333333333333333333333333333333";
const TAG_OBJECT: &str = "4444444444444444444444444444444444444444";

fn resolved(tag: Option<&str>, advertised_object_id: &str) -> ResolvedWorkflowRelease {
    ResolvedWorkflowRelease {
        tag: tag.map(str::to_owned),
        version: tag.map(|tag| Version::parse(tag.strip_prefix('v').unwrap_or(tag)).unwrap()),
        advertised_object_id: advertised_object_id.to_owned(),
    }
}

#[test]
fn resolves_release_variants() {
    let sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    for (output, tag, advertised_object_id) in [
        (
            format!("{FIRST}\trefs/tags/v1.2.3\n"),
            Some("v1.2.3"),
            FIRST,
        ),
        (
            format!(
                "{HEAD}\tHEAD\n{TAG_OBJECT}\trefs/tags/v2.0.0\n{SECOND}\trefs/tags/v2.0.0^{{}}\n{FIRST}\trefs/tags/v9.0.0-rc.1\n"
            ),
            Some("v2.0.0"),
            SECOND,
        ),
        (
            format!(
                "{HEAD}\tHEAD\n{FIRST}\trefs/tags/v1.10.0+one\n{TAG_OBJECT}\trefs/tags/1.10.0+two\n{SECOND}\trefs/tags/2.0.0+build.7\n"
            ),
            Some("2.0.0+build.7"),
            SECOND,
        ),
        (
            format!(
                "{FIRST}\trefs/tags/v1.2.3\n{FIRST}\trefs/tags/1.2.3\n{FIRST}\trefs/tags/v1.2.3\n"
            ),
            Some("1.2.3"),
            FIRST,
        ),
        (format!("{sha256}\tHEAD\n"), None, sha256),
        (
            format!(
                "{SECOND}\trefs/remotes/origin/HEAD\n{HEAD}\trefs/heads/main\n{FIRST}\trefs/tags/v1.2.3\n"
            ),
            Some("v1.2.3"),
            FIRST,
        ),
    ] {
        assert_eq!(
            resolve_workflow_release(&output).unwrap(),
            resolved(tag, advertised_object_id)
        );
    }
}

#[test]
fn rejects_invalid_listings() {
    for oid in [
        "111111111111111111111111111111111111111",
        "11111111111111111111111111111111111111111",
        "gggggggggggggggggggggggggggggggggggggggg",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaag",
        "0000000000000000000000000000000000000000",
        "0000000000000000000000000000000000000000000000000000000000000000",
    ] {
        assert!(resolve_workflow_release(&format!("{oid}\tHEAD\n")).is_err());
    }
    assert!(resolve_workflow_release("bad\trefs/remotes/origin/HEAD\n").is_err());

    for output in [
        String::new(),
        format!("{FIRST}\trefs/tags/v1.0.0-alpha.1\n"),
        format!("{FIRST}\trefs/tags/v1.2.3\n{SECOND}\trefs/tags/1.2.3\n"),
        format!("{FIRST}\trefs/tags/1.2.3+one\n{FIRST}\trefs/tags/v1.2.3+two\n"),
        format!("{HEAD}\tHEAD\n{FIRST}\tHEAD\n"),
        format!("{HEAD}\tHEAD\n{}\trefs/tags/v1.0.0\n", "a".repeat(64)),
        format!("{FIRST}\trefs/tags/v1.0.0\n{SECOND}\trefs/tags/v1.0.0\n"),
        format!(
            "{TAG_OBJECT}\trefs/tags/v1.0.0\n{FIRST}\trefs/tags/v1.0.0^{{}}\n{SECOND}\trefs/tags/v1.0.0^{{}}\n"
        ),
        format!("{FIRST}\trefs/tags/v1.0.0^{{}}\n"),
        format!("{HEAD} HEAD\n"),
        format!("{HEAD}\tHEAD\textra\n"),
        format!("{HEAD}\trefs/tags/\n"),
        format!("{HEAD}\trefs/tags/.hidden\n"),
        format!("{HEAD}\trefs/tags/bad..tag\n"),
        format!("{HEAD}\trefs/tags/v1.0.0^{{}}^{{}}\n"),
    ] {
        assert!(resolve_workflow_release(&output).is_err(), "{output}");
    }
}

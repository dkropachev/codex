use pretty_assertions::assert_eq;

use super::*;

#[test]
fn parses_comments_and_trailing_commas_without_changing_strings() {
    let parsed = parse_unique_jsonc(
        r#"{
            // line comment
            "url": "https://example.test/a//b",
            "comment": "/* data */",
            "quote": "escaped \" // text",
            "values": [true, null, -2, 3.5,],
            /* block
               comment */
        }"#,
    )
    .expect("valid JSONC");

    assert_eq!(
        parsed,
        serde_json::json!({
            "url": "https://example.test/a//b",
            "comment": "/* data */",
            "quote": "escaped \" // text",
            "values": [true, null, -2, 3.5]
        })
    );
}

#[test]
fn rejects_duplicate_keys_at_every_depth() {
    for contents in [
        r#"{"key": 1, "key": 2}"#,
        r#"{"items": [{"key": 1, "\u006bey": 2}]}"#,
    ] {
        let error = parse_unique_jsonc(contents).expect_err("duplicate key");
        assert!(format!("{error:#}").contains("duplicate object key `key`"));
    }
}

#[test]
fn rejects_malformed_or_broader_javascript_syntax() {
    for contents in [
        "{/* unterminated",
        r#"{"unterminated": "string}"#,
        r#"{unquoted: "key"}"#,
        r#"{'single': 'quotes'}"#,
        "{,}",
        "[,]",
    ] {
        assert!(
            parse_unique_jsonc(contents).is_err(),
            "accepted malformed input {contents:?}"
        );
    }
}

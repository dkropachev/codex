use super::*;

#[test]
fn stage_schemas_are_strict_and_bounded_where_required() {
    let discovery = discovery_schema();
    let verification = verification_schema();
    let fix_scope = fix_scope_schema();
    let fix = fix_schema();

    assert_eq!(discovery["additionalProperties"], false);
    assert_eq!(verification["additionalProperties"], false);
    assert_eq!(fix_scope["additionalProperties"], false);
    assert_eq!(fix["additionalProperties"], false);
    assert_eq!(
        fix["properties"]["resolution"]["properties"]["summary"]["maxItems"],
        5
    );
}

#[test]
fn discovery_validation_rejects_semantically_invalid_values() {
    let invalid = serde_json::json!({
        "candidates": [{
            "title": "Bad candidate",
            "body": "Body",
            "confidenceScore": 2.0,
            "priority": 99,
            "codeLocation": {
                "absoluteFilePath": "src/lib.rs",
                "lineRange": {"start": 0, "end": 0}
            }
        }],
        "assessment": {
            "verdict": "banana",
            "explanation": "Invalid",
            "confidenceScore": 2.0
        },
        "reviewContext": [],
        "externalReferences": []
    });

    assert!(validate(&invalid, &discovery_schema()).is_err());
}

use serde::de::DeserializeOwned;

use super::*;
use crate::tasks::review::output::DiscoveryOutput;
use crate::tasks::review::output::FixOutput;
use crate::tasks::review::output::FixScopeOutput;
use crate::tasks::review::output::VerificationOutput;

fn assessment() -> Value {
    json!({
        "verdict": "patch is correct",
        "explanation": "No findings.",
        "confidenceScore": 0.9
    })
}

fn code_location() -> Value {
    json!({
        "absoluteFilePath": "/repo/src/lib.rs",
        "lineRange": {"start": 1, "end": 2}
    })
}

fn discovery() -> Value {
    json!({
        "candidates": [{
            "title": "Handle failure",
            "body": "The failure is ignored.",
            "confidenceScore": 0.9,
            "priority": 1,
            "codeLocation": code_location()
        }],
        "assessment": assessment(),
        "reviewContext": [code_location()],
        "externalReferences": [{
            "reference": "issue tracker",
            "explanation": "Not loaded"
        }]
    })
}

fn assert_valid<T: DeserializeOwned>(value: &Value, schema: &Value) {
    validate(value, schema).expect("value should match stage schema");
    serde_json::from_value::<T>(value.clone()).expect("schema should match Rust output type");
}

#[test]
fn stage_schemas_accept_values_that_deserialize_to_their_output_types() {
    let mut discovery = discovery();
    assert_valid::<DiscoveryOutput>(&discovery, &discovery_schema());
    discovery["reviewContext"][0]["lineRange"] = json!({"start": u32::MAX, "end": u32::MAX});
    assert_valid::<DiscoveryOutput>(&discovery, &discovery_schema());

    let verification = json!({
        "findings": [{
            "candidateIndex": 0,
            "preExisting": "false",
            "preExistingFixRationale": null
        }],
        "outOfScopeFindings": [],
        "unverifiedFindings": [],
        "rejectedCandidateIndices": [],
        "assessment": assessment()
    });
    assert_valid::<VerificationOutput>(&verification, &verification_schema());

    let fix_scope = json!({
        "hasComparisonBaseline": true,
        "classifications": [{
            "findingIndex": 0,
            "preExisting": "false",
            "preExistingFixRationale": null,
            "validity": "valid"
        }]
    });
    assert_valid::<FixScopeOutput>(&fix_scope, &fix_scope_schema());

    let fix = json!({
        "classificationUpdates": [{
            "findingIndex": 0,
            "preExisting": "false",
            "preExistingFixRationale": null,
            "disposition": "fixed"
        }],
        "resolution": {
            "status": "complete",
            "fixedCount": 1,
            "rejectedCount": 0,
            "unresolvedCount": 0,
            "summary": ["Fixed the finding."],
            "tests": [{"command": "just test", "status": "passed"}],
            "commitSha": null
        }
    });
    assert_valid::<FixOutput>(&fix, &fix_schema());
}

#[test]
fn discovery_validation_enforces_each_supported_constraint() {
    let mut missing_required = discovery();
    missing_required
        .as_object_mut()
        .expect("discovery object")
        .remove("assessment");

    let mut unknown_field = discovery();
    unknown_field["unexpected"] = json!(true);

    let mut too_many_items = discovery();
    too_many_items["candidates"] = Value::Array(vec![discovery()["candidates"][0].clone(); 65]);

    let mut long_title = discovery();
    long_title["candidates"][0]["title"] = json!("x".repeat(MAX_TITLE_STRING + 1));

    let mut invalid_enum = discovery();
    invalid_enum["assessment"]["verdict"] = json!("banana");

    let mut reversed_range = discovery();
    reversed_range["reviewContext"][0]["lineRange"] = json!({"start": 2, "end": 1});

    let mut overflowing_range = discovery();
    overflowing_range["reviewContext"][0]["lineRange"]["end"] = json!(u64::from(u32::MAX) + 1);

    for invalid in [
        missing_required,
        unknown_field,
        too_many_items,
        long_title,
        invalid_enum,
        reversed_range,
        overflowing_range,
    ] {
        assert!(validate(&invalid, &discovery_schema()).is_err());
    }
}

#[test]
fn stage_specific_enums_are_rejected() {
    let invalid_verification = json!({
        "findings": [{
            "candidateIndex": 0,
            "preExisting": "sometimes",
            "preExistingFixRationale": null
        }],
        "outOfScopeFindings": [],
        "unverifiedFindings": [],
        "rejectedCandidateIndices": [],
        "assessment": assessment()
    });
    assert!(validate(&invalid_verification, &verification_schema()).is_err());

    let invalid_fix_scope = json!({
        "hasComparisonBaseline": true,
        "classifications": [{
            "findingIndex": 0,
            "preExisting": "false",
            "preExistingFixRationale": null,
            "validity": "maybe"
        }]
    });
    assert!(validate(&invalid_fix_scope, &fix_scope_schema()).is_err());

    let invalid_fix = json!({
        "classificationUpdates": [{
            "findingIndex": 0,
            "preExisting": "false",
            "preExistingFixRationale": null,
            "disposition": "ignored"
        }],
        "resolution": {
            "status": "complete",
            "fixedCount": 0,
            "rejectedCount": 0,
            "unresolvedCount": 1,
            "summary": [],
            "tests": [],
            "commitSha": null
        }
    });
    assert!(validate(&invalid_fix, &fix_schema()).is_err());
}

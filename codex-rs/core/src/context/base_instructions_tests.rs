use super::*;
use codex_protocol::models::BaseInstructions;
use pretty_assertions::assert_eq;

#[test]
fn fitting_items_preserve_complete_wire_representation_and_identity() -> serde_json::Result<()> {
    let namespace = Uuid::new_v5(&Uuid::NAMESPACE_OID, b"thread");
    let overhead = {
        let mut item = ContextualUserFragment::into(BaseInstructionsFragment(String::new()));
        item.set_id(Some(ResponseItemId::with_suffix("msg", namespace)));
        serialized_json_bytes(&item)?
    };
    let limit = approx_bytes_for_tokens(MAX_MODEL_CONTEXT_ITEM_TOKENS);
    for text in [
        BaseInstructions::default().text,
        "x".repeat(/*n*/ limit - overhead),
    ] {
        let mut expected = ContextualUserFragment::into(BaseInstructionsFragment(text.clone()));
        expected.set_id(Some(ResponseItemId::with_suffix(
            "msg",
            Uuid::new_v5(&namespace, text.as_bytes()),
        )));
        assert_eq!(
            BaseInstructionsFragment(text).into_bounded_items(namespace)?,
            vec![expected]
        );
    }
    Ok(())
}

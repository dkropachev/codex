use super::ContextualUserFragment;

pub(crate) const CYBER_POLICY_AUTO_RECOVERY_OPEN_TAG: &str = "<cyber_policy_auto_recovery>";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CyberPolicyAutoRecovery;

impl ContextualUserFragment for CyberPolicyAutoRecovery {
    fn role(&self) -> &'static str {
        "developer"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        (
            CYBER_POLICY_AUTO_RECOVERY_OPEN_TAG,
            "</cyber_policy_auto_recovery>",
        )
    }

    fn body(&self) -> String {
        concat!(
            "\nPrevious sampling request was blocked by cyber safety checks. Continue the turn ",
            "without repeating the blocked approach. Choose a policy-compliant path that advances ",
            "the user's underlying goal, limiting content to benign, defensive, educational, or ",
            "high-level assistance. Do not provide details that enable harmful activity or attempt ",
            "to bypass safeguards. If no compliant path can complete the request, explain the ",
            "limitation and offer the closest safe alternative.\n"
        )
        .to_string()
    }
}

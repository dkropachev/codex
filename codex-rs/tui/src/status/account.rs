#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StatusAccountDisplay {
    ChatGpt {
        email: Option<String>,
        plan: Option<String>,
    },
    ChatGptPool {
        pool_id: String,
        active_member: Option<StatusAccountPoolMemberDisplay>,
        member_resources: Vec<String>,
        member_count: usize,
        unavailable_count: usize,
    },
    ApiKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StatusAccountPoolMemberDisplay {
    pub(crate) id: String,
    pub(crate) email: Option<String>,
    pub(crate) plan: Option<String>,
}

pub(crate) fn pool_member_resources(
    member: &codex_app_server_protocol::AccountPoolMember,
) -> String {
    let resets = member
        .rate_limit_reset_credit_count
        .map(|count| count.to_string())
        .unwrap_or_else(|| "?".to_string());
    let credits = match member.credits.as_ref() {
        None => "?",
        Some(credits) if credits.unlimited => "unlimited",
        Some(credits) => credits
            .balance
            .as_deref()
            .unwrap_or(if credits.has_credits {
                "hidden (available)"
            } else {
                "hidden (unavailable)"
            }),
    };
    format!("{}: resets {resets}, credits {credits}", member.id)
}

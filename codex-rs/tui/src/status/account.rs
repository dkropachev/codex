#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StatusAccountDisplay {
    ChatGpt {
        email: Option<String>,
        plan: Option<String>,
    },
    ChatGptPool {
        pool_id: String,
        active_member: Option<StatusAccountPoolMemberDisplay>,
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

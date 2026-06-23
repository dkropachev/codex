use super::manager::CodexAuth;

pub const DEFAULT_ACCOUNT_POOL_AFFINITY_KEY: &str = "default";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AccountPoolUsageBucket {
    Regular,
    Spark,
}

pub type AccountPoolBucket = AccountPoolUsageBucket;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountPoolOperationKind {
    Stream,
    RealtimeSetup,
    Prewarm,
    Compaction,
    MemorySummarize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AccountPoolCacheHint {
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
}

impl AccountPoolCacheHint {
    pub fn heat(&self) -> AccountPoolCacheHeat {
        let input_tokens = self.input_tokens.max(0);
        let cached_input_tokens = self.cached_input_tokens.max(0);
        let cached_ratio = if input_tokens == 0 {
            0.0
        } else {
            cached_input_tokens as f64 / input_tokens as f64
        };
        if cached_input_tokens >= 10_000 || cached_ratio >= 0.50 {
            AccountPoolCacheHeat::Hot
        } else if cached_input_tokens < 1_000 && cached_ratio < 0.20 {
            AccountPoolCacheHeat::Cold
        } else {
            AccountPoolCacheHeat::Warm
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountPoolCacheHeat {
    Cold,
    Warm,
    Hot,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountPoolSelectionContext {
    pub bucket: AccountPoolUsageBucket,
    pub affinity_key: String,
    pub operation_kind: AccountPoolOperationKind,
    pub cache_hint: Option<AccountPoolCacheHint>,
    pub pinned_account_id: Option<String>,
}

impl AccountPoolSelectionContext {
    pub fn default_for_bucket(bucket: AccountPoolUsageBucket) -> Self {
        Self {
            bucket,
            affinity_key: DEFAULT_ACCOUNT_POOL_AFFINITY_KEY.to_string(),
            operation_kind: AccountPoolOperationKind::Stream,
            cache_hint: None,
            pinned_account_id: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AccountPoolAuthSelection {
    pub auth: CodexAuth,
    pub pool_id: String,
    pub account_id: String,
    pub bucket: AccountPoolUsageBucket,
    pub affinity_key: String,
    pub cache_hint: Option<AccountPoolCacheHint>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct AccountPoolAssignmentKey {
    pub pool_id: String,
    pub bucket: AccountPoolUsageBucket,
    pub affinity_key: String,
}

impl AccountPoolAssignmentKey {
    pub(crate) fn new(
        pool_id: String,
        bucket: AccountPoolUsageBucket,
        affinity_key: String,
    ) -> Self {
        Self {
            pool_id,
            bucket,
            affinity_key,
        }
    }
}

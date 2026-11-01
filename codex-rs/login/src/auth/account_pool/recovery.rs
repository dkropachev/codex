//! Error-triggered reset recovery, serialized by credential home across pool references.
use super::super::account_pool_selection::AccountPoolRecoveryMode;
use super::super::account_pool_selection::AccountPoolRecoveryState;
use super::routing::CapacityTier;
use super::transport::UsageOperation;
use super::usage::fetch_usage_remaining;
use super::*;
use std::path::Path;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::Weak;

const RECOVERY_TIMEOUT: Duration = Duration::from_secs(15);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Default)]
pub(super) struct ResetGuard {
    pub(super) state: tokio::sync::Mutex<ResetState>,
    pub(super) recovered_at: RwLock<HashMap<AccountPoolUsageBucket, Instant>>,
}
static RESET_GUARDS: LazyLock<Mutex<HashMap<PathBuf, Weak<ResetGuard>>>> =
    LazyLock::new(Mutex::default);

#[derive(Debug, Default)]
pub(super) struct ResetState {
    pub(super) owner: Option<String>,
    pub(super) pending: Option<PendingReset>,
    completed_at: Option<Instant>,
    attempted_at: Option<Instant>,
    blocked: bool,
}

#[derive(Debug)]
pub(super) struct PendingReset {
    request_id: String,
    pub(super) attempts: u8,
    pub(super) bucket: AccountPoolUsageBucket,
}

pub(super) fn reset_guard(home: &Path) -> Arc<ResetGuard> {
    let home = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
    let mut guards = RESET_GUARDS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guards.retain(|_, guard| guard.strong_count() > 0);
    if let Some(guard) = guards.get(&home).and_then(Weak::upgrade) {
        return guard;
    }
    let guard = Arc::new(ResetGuard::default());
    guards.insert(home, Arc::downgrade(&guard));
    guard
}

impl AccountPoolManager {
    pub(in crate::auth) async fn recover_selection(
        &self,
        selection: &AccountPoolAuthSelection,
        bucket: AccountPoolUsageBucket,
        mode: AccountPoolRecoveryMode,
        state: &mut AccountPoolRecoveryState,
    ) -> bool {
        let Some(pool) = self.pools.get(&selection.pool_id) else {
            return false;
        };
        if mode == AccountPoolRecoveryMode::SameAccountOnly && !pool.definition.auto_consume_resets
        {
            return false;
        }
        pool.mark_exhausted(
            &selection.account_id,
            bucket,
            "usage limit reached".to_string(),
        );
        // Spark errors also exhaust ordinary capacity, as in the existing failover path.
        if bucket == AccountPoolUsageBucket::Spark {
            pool.mark_exhausted(
                &selection.account_id,
                AccountPoolUsageBucket::Regular,
                "usage limit reached".to_string(),
            );
        }
        let can_switch = mode == AccountPoolRecoveryMode::AllowSwitch;
        let context = AccountPoolSelectionContext {
            bucket,
            affinity_key: selection.affinity_key.clone(),
            operation_kind: AccountPoolOperationKind::Stream,
            cache_hint: selection.cache_hint,
            pinned_account_id: None,
        };
        state.retry_selection = None;
        state.failed_selections.insert(
            (
                selection.pool_id.clone(),
                selection.account_id.clone(),
                bucket,
            ),
            selection.clone(),
        );
        let candidates = pool
            .members
            .iter()
            .filter_map(|member| {
                if !can_switch && member.account_id != selection.account_id {
                    return None;
                }
                if !member
                    .manager
                    .auth_cached_unpooled()
                    .is_some_and(|auth| auth.is_chatgpt_auth())
                {
                    return None;
                }
                let key = (pool.pool_id.clone(), member.account_id.clone());
                pool.member_tier(&member.account_id, bucket)
                    .or_else(|| {
                        (pool.definition.auto_consume_resets
                            && state.failed_selections.contains_key(&(
                                pool.pool_id.clone(),
                                member.account_id.clone(),
                                bucket,
                            ))
                            && !state.reset_attempts.contains(&key)
                            && pool
                                .member_state(&member.account_id)
                                .and_then(|state| state.usage)
                                .is_none_or(|usage| {
                                    usage
                                        .rate_limit_reset_credit_count
                                        .is_none_or(|count| count > 0)
                                }))
                        .then_some(CapacityTier::Resets)
                    })
                    .map(|tier| (member.account_id.clone(), tier))
            })
            .collect();
        for account_id in pool.order_member_ids(&context, selection.cache_hint, candidates) {
            if pool.member_tier(&account_id, bucket).is_some() {
                // Selection can also consider uncertain resets from earlier turns. Pin
                // this candidate so a reset already attempted this turn cannot win again.
                let mut retry_context = context.clone();
                retry_context.pinned_account_id = Some(account_id);
                state.retry_selection = pool.select_auth(retry_context).await;
                return state.retry_selection.is_some();
            }
            let key = (pool.pool_id.clone(), account_id.clone());
            let Some(failed) =
                state
                    .failed_selections
                    .get(&(pool.pool_id.clone(), account_id.clone(), bucket))
            else {
                // This member has not produced the error needed for reset redemption.
                return can_switch;
            };
            if !pool.definition.auto_consume_resets || !state.reset_attempts.insert(key) {
                continue;
            }
            let recovered =
                tokio::time::timeout(RECOVERY_TIMEOUT, pool.reset_after_error(failed, bucket))
                    .await
                    .unwrap_or(false);
            if recovered {
                state.retry_selection = Some(failed.clone());
                return true;
            }
        }
        false
    }
}

impl AccountPool {
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "serialize usage rechecks and reset redemption across pools sharing a credential home"
    )]
    async fn reset_after_error(
        &self,
        selection: &AccountPoolAuthSelection,
        bucket: AccountPoolUsageBucket,
    ) -> bool {
        let Some(member) = self.member(&selection.account_id) else {
            return false;
        };
        let Some(base_url) = self.chatgpt_base_url.as_deref() else {
            return false;
        };
        let mut guard = member.reset_guard.state.lock().await;
        let Some(auth) = member_auth(&member.account_id, &member.manager).await else {
            return false;
        };
        if auth.get_account_id() != selection.auth.get_account_id() {
            return false;
        }
        if guard.owner != auth.get_account_id() {
            *guard = ResetState {
                owner: auth.get_account_id(),
                ..Default::default()
            };
        }
        // Recheck after acquiring the home guard: another pool may already have recovered it.
        let Ok(Ok(usage)) = tokio::time::timeout(
            REQUEST_TIMEOUT,
            fetch_usage_remaining(base_url, &auth, &self.auth_route_config),
        )
        .await
        else {
            return false;
        };
        let included = usage.included_available(bucket);
        let resets = usage.rate_limit_reset_credit_count.unwrap_or(0);
        self.apply_usage(&selection.account_id, usage);
        if included == Some(true) {
            guard.pending = None;
            guard.blocked = false;
            self.clear_last_error(&selection.account_id);
            return true;
        }
        // Passive refresh can confirm recovery between exhausted periods. Keep that
        // evidence separately from the latest (possibly exhausted) usage snapshot.
        if let Some(recovered_at) = member
            .reset_guard
            .recovered_at
            .read()
            .ok()
            .and_then(|recovered| recovered.get(&bucket).copied())
            && guard
                .attempted_at
                .is_some_and(|attempted| recovered_at > attempted)
        {
            guard.pending = None;
            guard.blocked = false;
            guard.completed_at = Some(recovered_at);
            guard.attempted_at = None;
        }
        if included != Some(false)
            || resets <= 0
            || guard.blocked
            || guard
                .completed_at
                .is_some_and(|completed| completed >= selection.selected_at)
        {
            return false;
        }
        // Keep the same request ID after an uncertain POST. Bound uncertain retries across turns.
        let pending = guard.pending.get_or_insert_with(|| PendingReset {
            request_id: format!("codex-pool-{:032x}", rand::random::<u128>()),
            attempts: 0,
            bucket,
        });
        if pending.attempts >= 2 || pending.bucket != bucket {
            return false;
        }
        pending.attempts += 1;
        let request_id = pending.request_id.clone();
        guard.attempted_at = Some(Instant::now());
        let response = tokio::time::timeout(
            REQUEST_TIMEOUT,
            super::transport::request(
                base_url,
                &auth,
                &self.auth_route_config,
                UsageOperation::ConsumeReset {
                    redeem_request_id: &request_id,
                },
            ),
        )
        .await;
        if let Ok(Ok(payload)) = response {
            // All definitive responses end this redemption. Only a usage refresh can prove recovery.
            if let Some("reset" | "already_redeemed" | "nothing_to_reset" | "no_credit") =
                payload.get("code").and_then(serde_json::Value::as_str)
            {
                guard.pending = None;
                guard.blocked = true;
                guard.completed_at = Some(Instant::now());
            }
        }
        let Ok(Ok(usage)) = tokio::time::timeout(
            REQUEST_TIMEOUT,
            fetch_usage_remaining(base_url, &auth, &self.auth_route_config),
        )
        .await
        else {
            return false;
        };
        let recovered = usage.included_available(bucket) == Some(true);
        self.apply_usage(&selection.account_id, usage);
        if recovered {
            guard.pending = None;
            guard.blocked = false;
            guard.completed_at = Some(Instant::now());
            self.clear_last_error(&selection.account_id);
        }
        recovered
    }
}

#[cfg(test)]
#[path = "recovery_tests.rs"]
mod tests;

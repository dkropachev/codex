//! Resource tiers and cache-aware ordering, shared by selection and error recovery.
use super::*;
use codex_config::config_toml::AccountPoolFallbackPriorityToml;
use std::cmp::Ordering;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CapacityTier {
    Included,
    Resets,
    Credits,
}

impl AccountPool {
    pub(super) fn tier_rank(&self, tier: CapacityTier) -> u8 {
        match (tier, self.definition.fallback_priority) {
            (CapacityTier::Included, _) => 0,
            (CapacityTier::Resets, AccountPoolFallbackPriorityToml::ResetsFirst)
            | (CapacityTier::Credits, AccountPoolFallbackPriorityToml::CreditsFirst) => 1,
            (CapacityTier::Resets, AccountPoolFallbackPriorityToml::CreditsFirst)
            | (CapacityTier::Credits, AccountPoolFallbackPriorityToml::ResetsFirst) => 2,
        }
    }

    pub(super) fn member_tier(
        &self,
        account_id: &str,
        bucket: AccountPoolUsageBucket,
    ) -> Option<CapacityTier> {
        if self.is_exhausted(account_id, bucket) {
            return None;
        }
        let usage = self.member_state(account_id).and_then(|state| state.usage);
        // Unknown data retains best-effort routing. Known exhaustion is never treated as unknown.
        let Some(usage) = usage else {
            return Some(CapacityTier::Included);
        };
        if usage.included_available(bucket) != Some(false) {
            return Some(CapacityTier::Included);
        }
        let resets = self.definition.auto_consume_resets
            && usage
                .rate_limit_reset_credit_count
                .is_some_and(|count| count > 0);
        let credits = self.definition.auto_consume_credits && usage.credits_available();
        match (resets, credits, self.definition.fallback_priority) {
            (true, true, AccountPoolFallbackPriorityToml::ResetsFirst) | (true, false, _) => {
                Some(CapacityTier::Resets)
            }
            (true, true, AccountPoolFallbackPriorityToml::CreditsFirst) | (false, true, _) => {
                Some(CapacityTier::Credits)
            }
            (false, false, _) => None,
        }
    }

    pub(super) fn select_member_ids(
        &self,
        context: &AccountPoolSelectionContext,
        cache_hint: Option<AccountPoolCacheHint>,
    ) -> Vec<String> {
        let bucket = context.bucket;
        let candidates: Vec<_> = self
            .members
            .iter()
            .filter_map(|member| {
                self.member_tier(&member.account_id, bucket)
                    .map(|tier| (member.account_id.clone(), tier))
            })
            .collect();
        self.order_member_ids(context, cache_hint, candidates)
    }

    pub(super) fn order_member_ids(
        &self,
        context: &AccountPoolSelectionContext,
        cache_hint: Option<AccountPoolCacheHint>,
        mut candidates: Vec<(String, CapacityTier)>,
    ) -> Vec<String> {
        let bucket = context.bucket;
        let assignment_key = self.assignment_key(bucket, &context.affinity_key);
        if let Some(pinned) = context.pinned_account_id.as_deref() {
            candidates.retain(|(account_id, _)| account_id == pinned);
        }
        candidates.sort_by(|(left, left_tier), (right, right_tier)| {
            self.tier_rank(*left_tier)
                .cmp(&self.tier_rank(*right_tier))
                .then_with(|| {
                    if self.definition.policy == AccountPoolPolicyToml::Drain {
                        return Ordering::Equal;
                    }
                    match left_tier {
                        CapacityTier::Included => self
                            .member_score(right, bucket, &assignment_key)
                            .cmp(&self.member_score(left, bucket, &assignment_key)),
                        CapacityTier::Resets => self
                            .member_state(right)
                            .and_then(|state| state.usage)
                            .and_then(|usage| usage.rate_limit_reset_credit_count)
                            .cmp(
                                &self
                                    .member_state(left)
                                    .and_then(|state| state.usage)
                                    .and_then(|usage| usage.rate_limit_reset_credit_count),
                            ),
                        CapacityTier::Credits => compare_credits(
                            self.member_state(right)
                                .and_then(|state| state.usage)
                                .and_then(|usage| usage.credits),
                            self.member_state(left)
                                .and_then(|state| state.usage)
                                .and_then(|usage| usage.credits),
                        ),
                    }
                })
        });
        if self.definition.policy == AccountPoolPolicyToml::LoadBalance
            && cache_hint
                .map(|hint| hint.heat())
                .unwrap_or(AccountPoolCacheHeat::Cold)
                != AccountPoolCacheHeat::Cold
            && let Some(assignment) = self.assignment(&assignment_key)
            && let Some(index) = candidates
                .iter()
                .position(|(id, _)| *id == assignment.account_id)
            && candidates
                .first()
                .is_some_and(|(_, tier)| *tier == candidates[index].1)
            && (candidates[index].1 != CapacityTier::Included
                || !self.should_rebalance_assignment(&assignment.account_id, bucket, cache_hint))
        {
            let active = candidates.remove(index);
            candidates.insert(0, active);
        }
        candidates.into_iter().map(|(id, _)| id).collect()
    }

    fn member_score(
        &self,
        account_id: &str,
        bucket: AccountPoolUsageBucket,
        assignment_key: &AccountPoolAssignmentKey,
    ) -> i64 {
        let (remaining, unknown_usage_penalty) = match self.fresh_remaining(account_id, bucket) {
            Some(remaining) => (remaining as i64, 0),
            None => (0, UNKNOWN_USAGE_PENALTY),
        };
        let active_affinity_penalty = self.active_assignment_count(account_id, assignment_key)
            as i64
            * ACTIVE_AFFINITY_PENALTY;
        remaining - unknown_usage_penalty - active_affinity_penalty
    }

    fn active_assignment_count(
        &self,
        account_id: &str,
        current_key: &AccountPoolAssignmentKey,
    ) -> usize {
        let Ok(guard) = self.assignments.read() else {
            return 0;
        };
        guard
            .iter()
            .filter(|(key, assignment)| key != &current_key && assignment.account_id == account_id)
            .count()
    }

    fn fresh_remaining(&self, account_id: &str, bucket: AccountPoolUsageBucket) -> Option<u64> {
        self.member_state(account_id)
            .and_then(|state| state.usage)
            .filter(|usage| usage.last_refreshed.elapsed() <= USAGE_REFRESH_INTERVAL)
            .and_then(|usage| match bucket {
                AccountPoolUsageBucket::Regular => usage.regular_remaining,
                AccountPoolUsageBucket::Spark => usage.spark_remaining,
            })
    }

    fn should_rebalance_assignment(
        &self,
        active_account_id: &str,
        bucket: AccountPoolUsageBucket,
        cache_hint: Option<AccountPoolCacheHint>,
    ) -> bool {
        let Some(active_remaining) = self.fresh_remaining(active_account_id, bucket) else {
            return false;
        };
        let (low_remaining, material_advantage) = match cache_hint
            .map(|hint| hint.heat())
            .unwrap_or(AccountPoolCacheHeat::Cold)
        {
            AccountPoolCacheHeat::Cold => return true,
            AccountPoolCacheHeat::Warm => (
                WARM_REBALANCE_LOW_REMAINING,
                WARM_REBALANCE_MATERIAL_ADVANTAGE,
            ),
            AccountPoolCacheHeat::Hot => (
                HOT_REBALANCE_LOW_REMAINING,
                HOT_REBALANCE_MATERIAL_ADVANTAGE,
            ),
        };
        if active_remaining > low_remaining {
            return false;
        }
        self.members
            .iter()
            .filter(|member| member.account_id != active_account_id)
            .filter(|member| {
                self.member_tier(&member.account_id, bucket) == Some(CapacityTier::Included)
            })
            .filter_map(|member| self.fresh_remaining(&member.account_id, bucket))
            .max()
            .is_some_and(|best_remaining| {
                best_remaining >= active_remaining.saturating_add(material_advantage)
            })
    }

    pub(super) fn has_available_member(&self, bucket: AccountPoolUsageBucket) -> bool {
        self.members.iter().any(|member| {
            self.member_tier(&member.account_id, bucket).is_some()
                && member
                    .manager
                    .auth_cached_unpooled()
                    .is_some_and(|auth| auth.is_chatgpt_auth())
        })
    }
}

fn compare_credits(
    left: Option<codex_protocol::protocol::CreditsSnapshot>,
    right: Option<codex_protocol::protocol::CreditsSnapshot>,
) -> Ordering {
    let unlimited = |credits: &Option<codex_protocol::protocol::CreditsSnapshot>| {
        credits.as_ref().is_some_and(|credits| credits.unlimited)
    };
    unlimited(&left).cmp(&unlimited(&right)).then_with(|| {
        let left = left
            .as_ref()
            .and_then(|credits| credits.balance.as_deref())
            .and_then(usage::decimal_parts);
        let right = right
            .as_ref()
            .and_then(|credits| credits.balance.as_deref())
            .and_then(usage::decimal_parts);
        match (left, right) {
            (Some((li, lf)), Some((ri, rf))) => li
                .len()
                .cmp(&ri.len())
                .then_with(|| li.cmp(ri))
                .then_with(|| lf.cmp(rf)),
            (Some(_), None) => Ordering::Greater,
            (None, Some(_)) => Ordering::Less,
            (None, None) => Ordering::Equal,
        }
    })
}

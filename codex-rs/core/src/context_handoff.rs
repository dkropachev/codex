use crate::context::ContextualUserFragment;
use crate::context::HandoffGuidance;
use crate::context::HandoffInventory;
use crate::session::session::Session;
use crate::session::step_context::StepContext;
use crate::session::turn_context::TurnContext;
use codex_config::config_toml::ContextHandoffToml;
use std::collections::BTreeMap;

const MAX_GUIDANCE_BYTES: usize = 4_096;

#[derive(Debug, Clone, PartialEq)]
pub struct ContextHandoffConfig {
    soft: Expr,
    mid: Expr,
    hard: Expr,
    pub soft_guidance: String,
    pub mid_guidance: String,
    pub summary_instructions: String,
    rates: BTreeMap<String, InputRate>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct InputRate {
    input_per_million: f64,
    cached_input_per_million: f64,
}

#[derive(Debug, Clone, PartialEq)]
enum Expr {
    Compare(Field, Op, f64),
    And(Box<Self>, Box<Self>),
    Or(Box<Self>, Box<Self>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Field {
    Tokens,
    Price,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Op {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stage {
    None,
    Soft,
    Mid,
    Hard,
}

#[derive(Default)]
pub(crate) struct HandoffTurnState {
    pub(crate) hard_reset_attempted: bool,
    pub(crate) uncached_after_reset: bool,
    pub(crate) last_sampled_model: Option<String>,
}

/// Measures the context already assembled for the next request and appends one
/// guidance fragment for a newly reached soft or mid stage.
pub(crate) async fn stage_before_request(
    sess: &Session,
    step: &StepContext,
    state: &HandoffTurnState,
) -> Stage {
    let turn = step.turn.as_ref();
    let Some(policy) = turn.config.context_handoff.as_ref() else {
        return Stage::None;
    };
    let base_instructions = sess.get_prompt_base_instructions().await;
    let history_tokens = sess
        .clone_history()
        .await
        .estimate_token_count_with_base_instructions(&base_instructions)
        .unwrap_or(0);
    let tool_tokens = step
        .tool_router
        .model_visible_specs()
        .iter()
        .filter_map(|tool| serde_json::to_string(tool).ok())
        .map(|tool| codex_utils_output_truncation::approx_token_count(&tool) as i64)
        .sum::<i64>();
    let tokens = history_tokens.saturating_add(tool_tokens);
    let switched_model = if let Some(model) = state.last_sampled_model.as_deref() {
        model != step.settings.model_info.slug
    } else {
        sess.previous_turn_settings()
            .await
            .is_some_and(|settings| settings.model != step.settings.model_info.slug)
    };
    let cached = if state.uncached_after_reset || switched_model {
        0
    } else {
        sess.token_usage_info().await.map_or(0, |usage| {
            let last = usage.last_token_usage;
            if last.input_tokens > 0 {
                tokens.saturating_mul(last.cached_input_tokens.max(0)) / last.input_tokens
            } else {
                0
            }
        })
    };
    let api_billed = turn
        .auth_manager
        .as_deref()
        .and_then(codex_login::AuthManager::auth_cached)
        .is_none_or(|auth| !auth.uses_codex_backend());
    let price = api_billed
        .then(|| {
            policy.price(
                &turn.config.model_provider_id,
                &step.settings.model_info.slug,
                step.settings.service_tier.as_deref().unwrap_or("default"),
                tokens,
                cached,
            )
        })
        .flatten();
    let stage = policy.stage(tokens, price);
    if matches!(stage, Stage::Soft | Stage::Mid) && sess.claim_handoff_guidance(stage).await {
        let guidance = if stage == Stage::Mid {
            &policy.mid_guidance
        } else {
            &policy.soft_guidance
        };
        let message = ContextualUserFragment::into(HandoffGuidance(format!(
            "{guidance}\n{}",
            policy.summary_instructions
        )));
        sess.record_conversation_items(
            turn,
            &step.settings.model_info,
            std::slice::from_ref(&message),
        )
        .await;
    }
    stage
}

impl ContextHandoffConfig {
    pub fn parse(config: &ContextHandoffToml) -> Result<Self, String> {
        let parse = |name: &str, input: &str| -> Result<Expr, String> {
            if input.len() > 1_024 {
                return Err(format!("context_handoff.{name} exceeds 1024 bytes"));
            }
            Parser::new(input)
                .parse()
                .map_err(|error| format!("context_handoff.{name}: {error}"))
        };
        let bounded = |name: &str, text: Option<&str>, default: &str| -> Result<String, String> {
            let text = text.unwrap_or(default);
            if text.len() > MAX_GUIDANCE_BYTES {
                return Err(format!(
                    "context_handoff.{name} exceeds {MAX_GUIDANCE_BYTES} bytes"
                ));
            }
            Ok(text.to_string())
        };
        let mut rates = BTreeMap::new();
        for (key, rate) in config.rates.iter().flatten() {
            if key.len() > 256
                || key.split('/').count() != 3
                || !rate.input_per_million.is_finite()
                || rate.input_per_million < 0.0
                || !rate.cached_input_per_million.is_finite()
                || rate.cached_input_per_million < 0.0
            {
                return Err(format!("invalid context_handoff rate for {key}"));
            }
            rates.insert(
                key.clone(),
                InputRate {
                    input_per_million: rate.input_per_million,
                    cached_input_per_million: rate.cached_input_per_million,
                },
            );
        }
        Ok(Self {
            soft: parse("soft_when", &config.soft_when)?,
            mid: parse("mid_when", &config.mid_when)?,
            hard: parse("hard_when", &config.hard_when)?,
            soft_guidance: bounded(
                "soft_guidance",
                config.soft_guidance.as_deref(),
                "Consider calling handoff_context when you reach a safe pause. Include all unfinished work in its summary.",
            )?,
            mid_guidance: bounded(
                "mid_guidance",
                config.mid_guidance.as_deref(),
                "Prepare to call handoff_context at the next safe point before the context grows further.",
            )?,
            summary_instructions: bounded(
                "summary_instructions",
                config.summary_instructions.as_deref(),
                "Summarize the active task, decisions, unfinished work, and live agent or process IDs.",
            )?,
            rates,
        })
    }

    pub(crate) fn stage(&self, tokens: i64, price: Option<f64>) -> Stage {
        if self.hard.evaluate(tokens, price) {
            Stage::Hard
        } else if self.mid.evaluate(tokens, price) {
            Stage::Mid
        } else if self.soft.evaluate(tokens, price) {
            Stage::Soft
        } else {
            Stage::None
        }
    }

    pub(crate) fn price(
        &self,
        provider: &str,
        model: &str,
        tier: &str,
        tokens: i64,
        cached: i64,
    ) -> Option<f64> {
        let key = format!("{provider}/{model}/{tier}");
        let rate = self
            .rates
            .get(&key)
            .copied()
            .or_else(|| bundled_rate(provider, model, tier))?;
        let tokens = tokens.max(0);
        let cached = cached.clamp(/*min*/ 0, tokens);
        let multiplier = if provider == "openai"
            && matches!(model, "gpt-6-astra" | "gpt-6.1-sol" | "gpt-6-luna")
            && tokens > 272_000
        {
            2.0
        } else {
            1.0
        };
        Some(
            ((tokens - cached) as f64 * rate.input_per_million
                + cached as f64 * rate.cached_input_per_million)
                * multiplier
                / 1_000_000.0,
        )
    }
}

pub(crate) async fn record_live_handles(sess: &Session, turn: &TurnContext) {
    let mut entries = Vec::new();
    if let Ok(agents) = sess
        .services
        .agent_control
        .list(
            sess.thread_id,
            turn.parent_thread_id,
            &turn.session_source,
            /*path_prefix*/ None,
        )
        .await
    {
        for agent in agents
            .into_iter()
            .filter(|agent| agent.thread_id != sess.thread_id)
            .take(/*n*/ 32)
        {
            let name = agent
                .metadata
                .agent_path
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| agent.thread_id.to_string());
            entries.push(format!("agent {name}: {:?}", agent.status));
        }
    }
    for process in sess
        .services
        .unified_exec_manager
        .list_processes()
        .await
        .into_iter()
        .take(/*n*/ 32)
    {
        entries.push(format!("process {}", process.process_id));
    }
    if entries.is_empty() {
        return;
    }
    let inventory = entries.join("\n");
    let inventory = codex_utils_output_truncation::truncate_text(
        &inventory,
        codex_utils_output_truncation::TruncationPolicy::Tokens(/*tokens*/ 1_000),
    );
    let item = ContextualUserFragment::into(HandoffInventory(inventory));
    sess.record_conversation_items(turn, turn.model_info(), std::slice::from_ref(&item))
        .await;
}

// Standard API rates published at https://developers.openai.com/api/docs/pricing
// (checked 2026-10-13). Unknown models and tiers deliberately have no estimate.
fn bundled_rate(provider: &str, model: &str, tier: &str) -> Option<InputRate> {
    if provider != "openai" {
        return None;
    }
    let (input, cached) = match model {
        "gpt-6-astra" => (5.0, 0.5),
        "gpt-6.1-sol" => (2.0, 0.1),
        "gpt-6-luna" => (0.05, 0.005),
        _ => return None,
    };
    let multiplier = match tier {
        "default" | "standard" => 1.0,
        "fast" | "priority" => 2.0,
        "ultrafast" if model == "gpt-6.1-sol" => 6.0,
        "batch" | "flex" => 0.5,
        _ => return None,
    };
    Some(InputRate {
        input_per_million: input * multiplier,
        cached_input_per_million: cached * multiplier,
    })
}

impl Expr {
    fn evaluate(&self, tokens: i64, price: Option<f64>) -> bool {
        match self {
            Self::Compare(field, op, value) => {
                let observed = match field {
                    Field::Tokens => Some(tokens as f64),
                    Field::Price => price,
                };
                observed.is_some_and(|observed| match op {
                    Op::Lt => observed < *value,
                    Op::Le => observed <= *value,
                    Op::Gt => observed > *value,
                    Op::Ge => observed >= *value,
                    Op::Eq => observed == *value,
                })
            }
            Self::And(left, right) => left.evaluate(tokens, price) && right.evaluate(tokens, price),
            Self::Or(left, right) => left.evaluate(tokens, price) || right.evaluate(tokens, price),
        }
    }
}

struct Parser<'a> {
    text: &'a str,
    position: usize,
}
impl<'a> Parser<'a> {
    fn new(text: &'a str) -> Self {
        Self { text, position: 0 }
    }
    fn rest(&self) -> &'a str {
        &self.text[self.position..]
    }
    fn whitespace(&mut self) {
        while self
            .rest()
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.position += 1;
        }
    }
    fn consume(&mut self, token: &str) -> bool {
        self.whitespace();
        if self.rest().starts_with(token) {
            self.position += token.len();
            true
        } else {
            false
        }
    }
    fn parse(mut self) -> Result<Expr, String> {
        let expr = self.or()?;
        self.whitespace();
        if self.rest().is_empty() {
            Ok(expr)
        } else {
            Err(format!("unexpected input at byte {}", self.position))
        }
    }
    fn or(&mut self) -> Result<Expr, String> {
        let mut expr = self.and()?;
        while self.consume("||") {
            expr = Expr::Or(Box::new(expr), Box::new(self.and()?));
        }
        Ok(expr)
    }
    fn and(&mut self) -> Result<Expr, String> {
        let mut expr = self.atom()?;
        while self.consume("&&") {
            expr = Expr::And(Box::new(expr), Box::new(self.atom()?));
        }
        Ok(expr)
    }
    fn atom(&mut self) -> Result<Expr, String> {
        if self.consume("(") {
            let expr = self.or()?;
            if !self.consume(")") {
                return Err("expected ')'".to_string());
            }
            return Ok(expr);
        }
        let field = if self.consume("context.size.tokens") {
            Field::Tokens
        } else if self.consume("context.size.price") {
            Field::Price
        } else {
            return Err("expected context.size.tokens or context.size.price".to_string());
        };
        let op = if self.consume(">=") {
            Op::Ge
        } else if self.consume("<=") {
            Op::Le
        } else if self.consume("==") {
            Op::Eq
        } else if self.consume(">") {
            Op::Gt
        } else if self.consume("<") {
            Op::Lt
        } else {
            return Err("expected comparison operator".to_string());
        };
        self.whitespace();
        let bytes = self.rest().as_bytes();
        let len = bytes
            .iter()
            .take_while(|byte| byte.is_ascii_digit() || **byte == b'.')
            .count();
        if len == 0 {
            return Err("expected nonnegative number".to_string());
        }
        let value = self.rest()[..len]
            .parse::<f64>()
            .map_err(|_| "invalid number")?;
        if !value.is_finite() {
            return Err("number must be finite".to_string());
        }
        self.position += len;
        Ok(Expr::Compare(field, op, value))
    }
}

#[cfg(test)]
#[path = "context_handoff_tests.rs"]
mod tests;

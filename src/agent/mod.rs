//! AI decision-making layer for tx_agent.
//!
//! The agent owns retry decisions after failures. When `OPENAI_API_KEY` is
//! present, it asks an OpenAI-compatible chat-completions endpoint for a
//! structured decision. Without a key, it falls back to deterministic reasoning
//! and still records the evidence and rationale used for the decision.

use serde::{Deserialize, Serialize};

use crate::{config::Config, jito::tip::TipData, lifecycle::FailureKind};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentContext {
    pub submission_id: String,
    pub attempt: u32,
    pub failure: FailureKind,
    pub failure_detail: String,
    pub current_slot: Option<u64>,
    pub leader_slot: Option<u64>,
    pub previous_tip_lamports: u64,
    pub blockhash_age_slots: Option<u64>,
    pub tip: TipData,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RetryAction {
    Retry,
    Hold,
    Abort,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryDecision {
    pub action: RetryAction,
    pub reason: String,
    pub refresh_blockhash: bool,
    pub tip_lamports: u64,
    pub max_wait_slots: u64,
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct AiAgent {
    api_key: Option<String>,
    model: String,
    http: reqwest::Client,
}

impl AiAgent {
    pub fn new(config: &Config) -> Self {
        Self {
            api_key: config.openai_api_key.clone(),
            model: config.openai_model.clone(),
            http: reqwest::Client::new(),
        }
    }

    pub fn choose_tip_lamports(&self, tip: &TipData, slot_pressure: f64, floor: u64) -> u64 {
        let pressure = slot_pressure.clamp(0.0, 1.0);
        let percentile_tip_sol = if pressure > 0.75 {
            tip.landed_tips_95th_percentile
        } else if pressure > 0.35 {
            tip.landed_tips_75th_percentile
        } else {
            tip.ema_landed_tips_50th_percentile
        };
        sol_to_lamports(percentile_tip_sol).max(floor)
    }

    pub async fn decide_retry(&self, ctx: &AgentContext) -> RetryDecision {
        if let Some(api_key) = &self.api_key {
            match self.decide_retry_with_llm(api_key, ctx).await {
                Ok(decision) => return decision,
                Err(err) => {
                    tracing::warn!("AI retry decision failed, using fallback reasoning: {err:?}");
                }
            }
        }
        self.fallback_retry(ctx)
    }

    async fn decide_retry_with_llm(
        &self,
        api_key: &str,
        ctx: &AgentContext,
    ) -> anyhow::Result<RetryDecision> {
        #[derive(Serialize)]
        struct Message<'a> {
            role: &'a str,
            content: String,
        }

        #[derive(Serialize)]
        struct Request<'a> {
            model: &'a str,
            messages: Vec<Message<'a>>,
            temperature: f32,
            response_format: serde_json::Value,
        }

        #[derive(Deserialize)]
        struct Response {
            choices: Vec<Choice>,
        }

        #[derive(Deserialize)]
        struct Choice {
            message: ChoiceMessage,
        }

        #[derive(Deserialize)]
        struct ChoiceMessage {
            content: String,
        }

        let prompt = format!(
            "You are controlling retries for a Solana Jito bundle sender. \
             Return only JSON matching: \
             {{\"action\":\"retry|hold|abort\",\"reason\":\"...\",\"refresh_blockhash\":true|false,\
             \"tip_lamports\":123,\"max_wait_slots\":3,\"source\":\"openai\"}}.\n\
             Evidence:\n{}",
            serde_json::to_string_pretty(ctx)?
        );

        let request = Request {
            model: &self.model,
            temperature: 0.1,
            messages: vec![
                Message {
                    role: "system",
                    content: "Make one operational retry decision. Prefer retry only when the evidence identifies a fixable cause.".to_string(),
                },
                Message {
                    role: "user",
                    content: prompt,
                },
            ],
            response_format: serde_json::json!({ "type": "json_object" }),
        };

        let response: Response = self
            .http
            .post("https://api.openai.com/v1/chat/completions")
            .bearer_auth(api_key)
            .json(&request)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let content = response
            .choices
            .first()
            .ok_or_else(|| anyhow::anyhow!("OpenAI response contained no choices"))?
            .message
            .content
            .clone();
        let mut decision: RetryDecision = serde_json::from_str(&content)?;
        decision.source = "openai".to_string();
        Ok(validate_decision(decision, ctx))
    }

    fn fallback_retry(&self, ctx: &AgentContext) -> RetryDecision {
        let p75 = sol_to_lamports(ctx.tip.landed_tips_75th_percentile);
        let p95 = sol_to_lamports(ctx.tip.landed_tips_95th_percentile);
        let increased_tip = ctx.previous_tip_lamports.saturating_mul(125) / 100;

        match ctx.failure {
            FailureKind::ExpiredBlockhash => RetryDecision {
                action: RetryAction::Retry,
                reason: "The observed failure is blockhash expiry, so the next attempt must rebuild with a fresh processed blockhash and a current Jito tip.".to_string(),
                refresh_blockhash: true,
                tip_lamports: increased_tip.max(p75).max(1_000),
                max_wait_slots: 3,
                source: "fallback_reasoner".to_string(),
            },
            FailureKind::FeeTooLow | FailureKind::BundleFailure => RetryDecision {
                action: RetryAction::Retry,
                reason: "The bundle was rejected or underbid, so retrying is reasonable only with a higher tip and the next Jito leader window.".to_string(),
                refresh_blockhash: true,
                tip_lamports: increased_tip.max(p95).max(1_000),
                max_wait_slots: 4,
                source: "fallback_reasoner".to_string(),
            },
            FailureKind::JitoRateLimited => RetryDecision {
                action: RetryAction::Hold,
                reason: "The block engine is rate limiting requests; holding avoids burning blockhash lifetime on immediate resubmission.".to_string(),
                refresh_blockhash: true,
                tip_lamports: increased_tip.max(p75).max(1_000),
                max_wait_slots: 6,
                source: "fallback_reasoner".to_string(),
            },
            FailureKind::ComputeExceeded | FailureKind::SimulationFailure => RetryDecision {
                action: RetryAction::Abort,
                reason: "The transaction itself failed simulation or compute constraints; tip and blockhash changes do not fix that class of failure.".to_string(),
                refresh_blockhash: false,
                tip_lamports: ctx.previous_tip_lamports,
                max_wait_slots: 0,
                source: "fallback_reasoner".to_string(),
            },
            FailureKind::Timeout | FailureKind::Unknown => RetryDecision {
                action: RetryAction::Hold,
                reason: "The evidence is inconclusive; waiting for fresher leader and tip data is safer than blind retry.".to_string(),
                refresh_blockhash: true,
                tip_lamports: increased_tip.max(p75).max(1_000),
                max_wait_slots: 3,
                source: "fallback_reasoner".to_string(),
            },
        }
    }
}

fn validate_decision(mut decision: RetryDecision, ctx: &AgentContext) -> RetryDecision {
    if decision.tip_lamports == 0 {
        decision.tip_lamports = ctx.previous_tip_lamports.max(1_000);
    }
    if ctx.failure == FailureKind::ExpiredBlockhash {
        decision.refresh_blockhash = true;
    }
    decision
}

fn sol_to_lamports(sol: f64) -> u64 {
    (sol.max(0.0) * 1_000_000_000.0).ceil() as u64
}

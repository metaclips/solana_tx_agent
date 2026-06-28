//! AI decision-making layer for the MCP transaction control plane.
//!
//! The agent owns one bounded operational decision for an already signed
//! transaction: submit now, wait for a leader, retry the same signed payload,
//! abandon, or escalate for a newly signed payload.

use serde::{Deserialize, Serialize};

use crate::{config::Config, jito::tip::TipData, lifecycle::FailureKind};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperationalAction {
    SubmitNow,
    WaitForLeader,
    Retry,
    Abandon,
    Escalate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationalContext {
    pub request_id: String,
    pub attempt: u32,
    pub current_slot: Option<u64>,
    pub leader_slot: Option<u64>,
    pub slots_until_leader: Option<u64>,
    pub recent_tip: TipData,
    pub previous_tip_lamports: u64,
    pub tip_floor_lamports: u64,
    pub max_tip_lamports: u64,
    pub can_refresh_blockhash: bool,
    pub can_adjust_tip: bool,
    pub failure: Option<AgentFailureReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentFailureReport {
    pub failure: FailureKind,
    pub detail: String,
    pub submitted_slot: Option<u64>,
    pub blockhash: String,
    pub processed_latency_ms: Option<u128>,
    pub confirmed_latency_ms: Option<u128>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationalDecision {
    pub action: OperationalAction,
    pub reason: String,
    pub refresh_blockhash: bool,
    pub tip_lamports: u64,
    pub max_wait_slots: u64,
    pub source: String,
}

impl OperationalDecision {
    pub fn is_write_action(&self) -> bool {
        matches!(
            self.action,
            OperationalAction::SubmitNow | OperationalAction::Retry
        )
    }
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

    pub async fn decide_operation(&self, ctx: &OperationalContext) -> OperationalDecision {
        if let Some(api_key) = &self.api_key {
            match self.decide_operation_with_llm(api_key, ctx).await {
                Ok(decision) => return decision,
                Err(err) => {
                    tracing::warn!(
                        "AI operational decision failed, using fallback reasoning: {err:?}"
                    );
                }
            }
        }
        self.fallback_operation(ctx)
    }

    async fn decide_operation_with_llm(
        &self,
        api_key: &str,
        ctx: &OperationalContext,
    ) -> anyhow::Result<OperationalDecision> {
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
            "You are the bounded operational agent for a Solana Jito transaction stack. \
             You do not sign transactions and cannot call Solana or Jito directly. \
             The transaction is already signed; if can_refresh_blockhash=false or can_adjust_tip=false, \
             do not choose retry as though those fields can be mutated. Escalate when a new signed transaction is needed. \
             Choose exactly one action from submit_now, wait_for_leader, retry, abandon, escalate. \
             Return only JSON matching: \
             {{\"action\":\"submit_now|wait_for_leader|retry|abandon|escalate\",\
             \"reason\":\"...\",\"refresh_blockhash\":true|false,\"tip_lamports\":123,\
             \"max_wait_slots\":3,\"source\":\"openai\"}}.\n\
             Evidence and policy limits:\n{}",
            serde_json::to_string_pretty(ctx)?
        );

        let request = Request {
            model: &self.model,
            temperature: 0.1,
            messages: vec![
                Message {
                    role: "system",
                    content: "Make one bounded Solana transaction operation decision. Balance landing probability, tip cost, leader timing, and failure evidence. Do not invent tools or request private-key access.".to_string(),
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
        let mut decision: OperationalDecision = serde_json::from_str(&content)?;
        decision.source = "openai".to_string();
        Ok(validate_operational_decision(decision, ctx))
    }

    fn fallback_operation(&self, ctx: &OperationalContext) -> OperationalDecision {
        let pressure = ctx
            .slots_until_leader
            .map(|slots| 1.0 - (slots as f64 / 3.0))
            .unwrap_or(0.0);
        let default_tip = self
            .choose_tip_lamports(&ctx.recent_tip, pressure, ctx.tip_floor_lamports)
            .min(ctx.max_tip_lamports);

        if let Some(failure) = &ctx.failure {
            match failure.failure {
                FailureKind::ExpiredBlockhash => OperationalDecision {
                    action: OperationalAction::Escalate,
                    reason: "The signed transaction's blockhash expired and this agent cannot refresh or re-sign it; a new signed transaction is required.".to_string(),
                    refresh_blockhash: false,
                    tip_lamports: ctx.previous_tip_lamports.max(ctx.tip_floor_lamports),
                    max_wait_slots: 0,
                    source: "fallback_reasoner".to_string(),
                },
                FailureKind::ComputeExceeded | FailureKind::SimulationFailure => {
                    OperationalDecision {
                        action: OperationalAction::Abandon,
                        reason: "The failure is transaction-shape related, not a timing or tip issue.".to_string(),
                        refresh_blockhash: false,
                        tip_lamports: ctx.previous_tip_lamports.max(ctx.tip_floor_lamports),
                        max_wait_slots: 0,
                        source: "fallback_reasoner".to_string(),
                    }
                }
                FailureKind::FeeTooLow | FailureKind::BundleFailure => {
                    let target_tip = sol_to_lamports(ctx.recent_tip.landed_tips_95th_percentile)
                        .max(default_tip)
                        .min(ctx.max_tip_lamports);
                    OperationalDecision {
                        action: OperationalAction::Escalate,
                        reason: "The signed transaction appears underbid or dropped, but this agent cannot change its embedded tip; a newly signed transaction with a higher tip is required.".to_string(),
                        refresh_blockhash: false,
                        tip_lamports: target_tip,
                        max_wait_slots: 0,
                        source: "fallback_reasoner".to_string(),
                    }
                }
                FailureKind::JitoRateLimited | FailureKind::Timeout | FailureKind::Unknown => {
                    OperationalDecision {
                        action: OperationalAction::WaitForLeader,
                        reason: "The evidence is inconclusive or rate-limited; wait for fresher leader and tip data before another write.".to_string(),
                        refresh_blockhash: false,
                        tip_lamports: default_tip,
                        max_wait_slots: 3,
                        source: "fallback_reasoner".to_string(),
                    }
                }
            }
        } else if ctx.slots_until_leader.unwrap_or(u64::MAX) <= 3 {
            OperationalDecision {
                action: OperationalAction::SubmitNow,
                reason: "A connected Jito leader is inside the near submission window.".to_string(),
                refresh_blockhash: false,
                tip_lamports: default_tip,
                max_wait_slots: 3,
                source: "fallback_reasoner".to_string(),
            }
        } else {
            OperationalDecision {
                action: OperationalAction::WaitForLeader,
                reason: "No near connected Jito leader is available yet.".to_string(),
                refresh_blockhash: false,
                tip_lamports: default_tip,
                max_wait_slots: 3,
                source: "fallback_reasoner".to_string(),
            }
        }
    }
}

fn validate_operational_decision(
    mut decision: OperationalDecision,
    ctx: &OperationalContext,
) -> OperationalDecision {
    if decision.tip_lamports == 0 {
        decision.tip_lamports = ctx.tip_floor_lamports.max(ctx.previous_tip_lamports);
    }
    if !ctx.can_refresh_blockhash {
        decision.refresh_blockhash = false;
    }
    decision
}

fn sol_to_lamports(sol: f64) -> u64 {
    (sol.max(0.0) * 1_000_000_000.0).ceil() as u64
}

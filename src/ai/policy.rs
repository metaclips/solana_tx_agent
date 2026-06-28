use serde::{Deserialize, Serialize};

use crate::{ai::agent::OperationalDecision, core::config::Config};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyLimits {
    pub min_tip_lamports: u64,
    pub max_tip_lamports: u64,
    pub max_retries: u32,
    pub max_wait_slots: u64,
}

impl PolicyLimits {
    pub fn from_config(config: &Config) -> Self {
        Self {
            min_tip_lamports: config.tip_floor_lamports,
            max_tip_lamports: config.max_agent_tip_lamports,
            max_retries: config.max_agent_retries,
            max_wait_slots: config.max_agent_wait_slots,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatedDecision {
    pub decision: OperationalDecision,
    pub limits: PolicyLimits,
}

pub fn validate_decision(
    mut decision: OperationalDecision,
    attempt: u32,
    limits: &PolicyLimits,
) -> anyhow::Result<ValidatedDecision> {
    if decision.tip_lamports < limits.min_tip_lamports {
        decision.tip_lamports = limits.min_tip_lamports;
    }

    if decision.tip_lamports > limits.max_tip_lamports {
        anyhow::bail!(
            "agent tip {} exceeds policy max {}",
            decision.tip_lamports,
            limits.max_tip_lamports
        );
    }

    if decision.max_wait_slots > limits.max_wait_slots {
        decision.max_wait_slots = limits.max_wait_slots;
    }

    if decision.is_write_action() && attempt > limits.max_retries {
        anyhow::bail!(
            "agent requested write action on attempt {attempt}, above max retries {}",
            limits.max_retries
        );
    }

    Ok(ValidatedDecision {
        decision,
        limits: limits.clone(),
    })
}

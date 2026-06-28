use std::{fs::OpenOptions, io::Write, path::PathBuf, sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use solana_sdk::signature::Signature;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CommitmentStage {
    Submitted,
    Accepted,
    Processed,
    Confirmed,
    Finalized,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum FailureKind {
    ExpiredBlockhash,
    FeeTooLow,
    ComputeExceeded,
    BundleFailure,
    JitoRateLimited,
    SimulationFailure,
    Timeout,
    Unknown,
}

impl FailureKind {
    pub fn classify(raw: &str) -> Self {
        let lower = raw.to_ascii_lowercase();
        if lower.contains("blockhash") || lower.contains("block height exceeded") {
            Self::ExpiredBlockhash
        } else if lower.contains("bid") || lower.contains("tip") || lower.contains("fee") {
            Self::FeeTooLow
        } else if lower.contains("compute") || lower.contains("cu") {
            Self::ComputeExceeded
        } else if lower.contains("simulation") {
            Self::SimulationFailure
        } else if lower.contains("rate") || lower.contains("resource exhausted") {
            Self::JitoRateLimited
        } else if lower.contains("timeout") {
            Self::Timeout
        } else if !raw.is_empty() {
            Self::BundleFailure
        } else {
            Self::Unknown
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageEvent {
    pub stage: CommitmentStage,
    pub at: DateTime<Utc>,
    pub slot: Option<u64>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifecycleRecord {
    pub submission_id: String,
    pub attempt: u32,
    pub bundle_id: Option<String>,
    pub signature: String,
    pub tip_lamports: u64,
    pub submitted_slot: Option<u64>,
    pub leader_slot: Option<u64>,
    pub blockhash: String,
    pub last_valid_block_height: Option<u64>,
    pub submitted_at: DateTime<Utc>,
    pub processed_at: Option<DateTime<Utc>>,
    pub confirmed_at: Option<DateTime<Utc>>,
    pub finalized_at: Option<DateTime<Utc>>,
    pub processed_slot: Option<u64>,
    pub confirmed_slot: Option<u64>,
    pub finalized_slot: Option<u64>,
    pub processed_latency_ms: Option<u128>,
    pub confirmed_latency_ms: Option<u128>,
    pub finalized_latency_ms: Option<u128>,
    pub failure: Option<FailureKind>,
    pub failure_detail: Option<String>,
    pub events: Vec<StageEvent>,
}

impl LifecycleRecord {
    pub fn new(
        submission_id: String,
        attempt: u32,
        signature: Signature,
        tip_lamports: u64,
        submitted_slot: Option<u64>,
        leader_slot: Option<u64>,
        blockhash: String,
        last_valid_block_height: Option<u64>,
    ) -> Self {
        let now = Utc::now();
        Self {
            submission_id,
            attempt,
            bundle_id: None,
            signature: signature.to_string(),
            tip_lamports,
            submitted_slot,
            leader_slot,
            blockhash,
            last_valid_block_height,
            submitted_at: now,
            processed_at: None,
            confirmed_at: None,
            finalized_at: None,
            processed_slot: None,
            confirmed_slot: None,
            finalized_slot: None,
            processed_latency_ms: None,
            confirmed_latency_ms: None,
            finalized_latency_ms: None,
            failure: None,
            failure_detail: None,
            events: vec![StageEvent {
                stage: CommitmentStage::Submitted,
                at: now,
                slot: submitted_slot,
                detail: None,
            }],
        }
    }

    pub fn add_stage(&mut self, stage: CommitmentStage, slot: Option<u64>, detail: Option<String>) {
        let now = Utc::now();
        match stage {
            CommitmentStage::Submitted => {}
            CommitmentStage::Accepted => {}
            CommitmentStage::Processed => {
                if self.processed_at.is_none() {
                    self.processed_at = Some(now);
                    self.processed_slot = slot;
                    self.processed_latency_ms = Some(delta_ms(self.submitted_at, now));
                }
            }
            CommitmentStage::Confirmed => {
                if self.confirmed_at.is_none() {
                    self.confirmed_at = Some(now);
                    self.confirmed_slot = slot.or(self.processed_slot);
                    self.confirmed_latency_ms = Some(delta_ms(self.submitted_at, now));
                }
            }
            CommitmentStage::Finalized => {
                if self.finalized_at.is_none() {
                    self.finalized_at = Some(now);
                    self.finalized_slot = slot.or(self.confirmed_slot).or(self.processed_slot);
                    self.finalized_latency_ms = Some(delta_ms(self.submitted_at, now));
                }
            }
        }
        self.events.push(StageEvent {
            stage,
            at: now,
            slot,
            detail,
        });
    }

    pub fn fail(&mut self, kind: FailureKind, detail: impl Into<String>) {
        self.failure = Some(kind);
        self.failure_detail = Some(detail.into());
    }

    pub fn is_terminal_success(&self) -> bool {
        self.finalized_at.is_some() || self.confirmed_at.is_some()
    }
}

fn delta_ms(start: DateTime<Utc>, end: DateTime<Utc>) -> u128 {
    end.signed_duration_since(start)
        .to_std()
        .unwrap_or_else(|_| Duration::from_millis(0))
        .as_millis()
}

#[derive(Clone)]
pub struct LifecycleLogger {
    path: Arc<PathBuf>,
    lock: Arc<Mutex<()>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentAuditRecord {
    pub request_id: String,
    pub attempt: u32,
    pub at: DateTime<Utc>,
    pub input_state: serde_json::Value,
    pub decision: serde_json::Value,
    pub policy: serde_json::Value,
    pub outcome: Option<serde_json::Value>,
}

impl LifecycleLogger {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: Arc::new(path.into()),
            lock: Arc::new(Mutex::new(())),
        }
    }

    pub async fn append(&self, record: &LifecycleRecord) -> anyhow::Result<()> {
        let _guard = self.lock.lock().await;
        if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            tokio::fs::create_dir_all(parent).await?;
        }
        let json = serde_json::to_string(record)?;
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let mut file = OpenOptions::new().create(true).append(true).open(&*path)?;
            writeln!(file, "{json}")?;
            Ok(())
        })
        .await??;
        Ok(())
    }

    pub async fn append_agent_audit(&self, record: &AgentAuditRecord) -> anyhow::Result<()> {
        let _guard = self.lock.lock().await;
        let audit_path = self.path.with_file_name("agent_decisions.log.jsonl");
        if let Some(parent) = audit_path.parent().filter(|p| !p.as_os_str().is_empty()) {
            tokio::fs::create_dir_all(parent).await?;
        }
        let json = serde_json::to_string(record)?;
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(audit_path)?;
            writeln!(file, "{json}")?;
            Ok(())
        })
        .await??;
        Ok(())
    }
}

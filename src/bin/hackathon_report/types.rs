use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize)]
pub(crate) struct HackathonReport {
    pub(crate) run_id: String,
    pub(crate) generated_at: DateTime<Utc>,
    pub(crate) run_started_at: DateTime<Utc>,
    pub(crate) run_finished_at: DateTime<Utc>,
    pub(crate) environment: ReportEnvironment,
    pub(crate) summary: ReportSummary,
    pub(crate) submissions: Vec<SubmissionReport>,
    pub(crate) lifecycle_records: Vec<Value>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ReportEnvironment {
    pub(crate) solana_rpc_url: String,
    pub(crate) yellowstone_endpoint: String,
    pub(crate) jito_block_engine_url: Option<String>,
    pub(crate) mcp_url: String,
    pub(crate) lifecycle_log_path: PathBuf,
    pub(crate) agent_log_path: PathBuf,
}

#[derive(Debug, Serialize)]
pub(crate) struct ReportSummary {
    pub(crate) total_submissions: usize,
    pub(crate) expected_successes: usize,
    pub(crate) expected_failures: usize,
    pub(crate) observed_successes: usize,
    pub(crate) observed_failures: usize,
    pub(crate) records_with_slots: usize,
    pub(crate) records_with_commitment_progression: usize,
    pub(crate) expected_outcomes_met: usize,
}

impl ReportSummary {
    pub(crate) fn from_submissions(submissions: &[SubmissionReport]) -> Self {
        Self {
            total_submissions: submissions.len(),
            expected_successes: submissions
                .iter()
                .filter(|submission| {
                    submission.transaction.expected_outcome == ExpectedOutcome::Success
                })
                .count(),
            expected_failures: submissions
                .iter()
                .filter(|submission| {
                    submission.transaction.expected_outcome == ExpectedOutcome::Failure
                })
                .count(),
            observed_successes: submissions
                .iter()
                .filter(|submission| {
                    submission
                        .validation
                        .as_ref()
                        .is_some_and(|validation| validation.observed_success)
                })
                .count(),
            observed_failures: submissions
                .iter()
                .filter(|submission| {
                    submission
                        .validation
                        .as_ref()
                        .is_some_and(|validation| validation.observed_failure)
                })
                .count(),
            records_with_slots: submissions
                .iter()
                .filter(|submission| {
                    submission
                        .validation
                        .as_ref()
                        .is_some_and(|validation| validation.slot_numbers.submitted_slot.is_some())
                })
                .count(),
            records_with_commitment_progression: submissions
                .iter()
                .filter(|submission| {
                    submission
                        .validation
                        .as_ref()
                        .is_some_and(|validation| validation.commitment_progression.len() > 1)
                })
                .count(),
            expected_outcomes_met: submissions
                .iter()
                .filter(|submission| {
                    submission
                        .validation
                        .as_ref()
                        .is_some_and(|validation| validation.expected_outcome_met)
                })
                .count(),
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct SubmissionReport {
    pub(crate) request_id: String,
    pub(crate) transaction: GeneratedTransaction,
    pub(crate) agent: AgentRunReport,
    pub(crate) lifecycle: Option<Value>,
    pub(crate) validation: Option<SubmissionValidation>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AgentRunReport {
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) finished_at: DateTime<Utc>,
    pub(crate) duration_ms: u128,
    pub(crate) exit_code: Option<i32>,
    pub(crate) success: bool,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct SubmissionValidation {
    pub(crate) matched_lifecycle_record: bool,
    pub(crate) observed_success: bool,
    pub(crate) observed_failure: bool,
    pub(crate) expected_outcome_met: bool,
    pub(crate) slot_numbers: SlotNumbers,
    pub(crate) commitment_progression: Vec<String>,
    pub(crate) failure_classification: Option<String>,
    pub(crate) failure_detail: Option<String>,
    pub(crate) notes: Vec<String>,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct SlotNumbers {
    pub(crate) submitted_slot: Option<u64>,
    pub(crate) leader_slot: Option<u64>,
    pub(crate) processed_slot: Option<u64>,
    pub(crate) confirmed_slot: Option<u64>,
    pub(crate) finalized_slot: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct GeneratedTransaction {
    pub(crate) request_id: String,
    pub(crate) kind: TransactionKind,
    pub(crate) expected_outcome: ExpectedOutcome,
    pub(crate) encoding: String,
    pub(crate) transaction_file: PathBuf,
    pub(crate) payer: String,
    pub(crate) tip_account: String,
    pub(crate) tip_lamports: u64,
    pub(crate) blockhash: String,
    pub(crate) signature: String,
    pub(crate) constructed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TransactionKind {
    FundedTipTransfer,
    UnfundedTipTransfer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExpectedOutcome {
    Success,
    Failure,
}

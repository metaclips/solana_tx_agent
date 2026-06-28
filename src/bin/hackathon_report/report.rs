use std::{env, fs, path::Path, time::Duration};

use anyhow::Context;
use chrono::Utc;
use serde_json::Value;
use solana_client::rpc_client::RpcClient;
use solana_commitment_config::CommitmentConfig;
use solana_sdk::signature::read_keypair_file;

use crate::{
    agent::{start_agent, submit_transaction_with_agent},
    config::ReportConfig,
    transactions::{generate_funded_tip_transaction, generate_unfunded_tip_transaction},
    types::{
        ExpectedOutcome, HackathonReport, ReportEnvironment, ReportSummary, SlotNumbers,
        SubmissionReport, SubmissionValidation,
    },
    util::wait_for_agent,
};

pub(crate) fn run_report(config: ReportConfig) -> anyhow::Result<HackathonReport> {
    let _ = fs::remove_file(&config.lifecycle_log_path);
    fs::create_dir_all(&config.work_dir)?;

    let mut agent = start_agent(&config)?;
    wait_for_agent(
        &config.mcp_bind_addr,
        Duration::from_secs(config.server_ready_timeout_secs),
        &mut agent,
        &config.agent_log_path,
    )?;

    let rpc = RpcClient::new_with_commitment(
        config.solana_rpc_url.clone(),
        CommitmentConfig::processed(),
    );
    let funded_payer = read_keypair_file(&config.payer_keypair_path).map_err(|err| {
        anyhow::anyhow!(
            "failed reading payer keypair {}: {err}",
            config.payer_keypair_path.display()
        )
    })?;

    let mut submissions = Vec::new();
    let run_started_at = Utc::now();

    for index in 1..=config.success_count {
        let request_id = format!("{}-success-{index:02}", config.request_prefix);
        let tx = generate_funded_tip_transaction(&rpc, &funded_payer, &config, &request_id)?;
        submissions.push(submit_transaction_with_agent(&config, tx)?);
    }

    for index in 1..=config.failure_count {
        let request_id = format!("{}-failure-unfunded-{index:02}", config.request_prefix);
        let tx = generate_unfunded_tip_transaction(&rpc, &config, &request_id)?;
        submissions.push(submit_transaction_with_agent(&config, tx)?);
    }

    let lifecycle_records = read_lifecycle_records(&config.lifecycle_log_path)?;
    attach_lifecycle_records(&mut submissions, &lifecycle_records);
    let summary = ReportSummary::from_submissions(&submissions);
    let mcp_url = config.mcp_url();

    Ok(HackathonReport {
        run_id: config.request_prefix,
        generated_at: Utc::now(),
        run_started_at,
        run_finished_at: Utc::now(),
        environment: ReportEnvironment {
            solana_rpc_url: "REDACTED".to_string(),
            yellowstone_endpoint: "REDACTED".to_string(),
            jito_block_engine_url: env::var("JITO_BLOCK_ENGINE_URL")
                .ok()
                .filter(|value| !value.is_empty())
                .map(|_| "REDACTED".to_string()),
            mcp_url,
            lifecycle_log_path: config.lifecycle_log_path,
            agent_log_path: config.agent_log_path,
        },
        summary,
        submissions,
        lifecycle_records,
    })
}

pub(crate) fn write_report(path: &Path, report: &HackathonReport) -> anyhow::Result<()> {
    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let mut value = serde_json::to_value(report)?;
    scrub_sensitive_json(&mut value);
    fs::write(path, serde_json::to_string_pretty(&value)?)?;
    Ok(())
}

fn scrub_sensitive_json(value: &mut Value) {
    match value {
        Value::String(text) => *text = scrub_sensitive_text(text),
        Value::Array(values) => values.iter_mut().for_each(scrub_sensitive_json),
        Value::Object(values) => values.values_mut().for_each(scrub_sensitive_json),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn scrub_sensitive_text(text: &str) -> String {
    let mut scrubbed = text.to_string();
    for name in [
        "SOLANA_RPC_URL",
        "YELLOWSTONE_ENDPOINT",
        "YELLOWSTONE_TOKEN",
        "JITO_AUTH_KEYPAIR",
        "TX_AGENT_REAL_PAYER_KEYPAIR",
        "PAYER_KEYPAIR",
        "OPENAI_API_KEY",
        "JITO_BLOCK_ENGINE_URL",
    ] {
        if let Ok(secret) = env::var(name) {
            if !secret.is_empty() {
                scrubbed = scrubbed.replace(&secret, "REDACTED");
            }
        }
    }
    scrubbed
}

fn attach_lifecycle_records(submissions: &mut [SubmissionReport], records: &[Value]) {
    for submission in submissions {
        let record = records
            .iter()
            .find(|record| record["submission_id"] == submission.request_id)
            .cloned();
        submission.validation = Some(validate_submission(submission, record.as_ref()));
        submission.lifecycle = record;
    }
}

fn validate_submission(
    submission: &SubmissionReport,
    lifecycle: Option<&Value>,
) -> SubmissionValidation {
    let Some(record) = lifecycle else {
        return SubmissionValidation {
            matched_lifecycle_record: false,
            observed_success: false,
            observed_failure: false,
            expected_outcome_met: false,
            slot_numbers: SlotNumbers::default(),
            commitment_progression: Vec::new(),
            failure_classification: None,
            failure_detail: None,
            notes: vec!["missing lifecycle record".to_string()],
        };
    };

    let slot_numbers = SlotNumbers {
        submitted_slot: record["submitted_slot"].as_u64(),
        leader_slot: record["leader_slot"].as_u64(),
        processed_slot: record["processed_slot"].as_u64(),
        confirmed_slot: record["confirmed_slot"].as_u64(),
        finalized_slot: record["finalized_slot"].as_u64(),
    };
    let commitment_progression = record["events"]
        .as_array()
        .map(|events| {
            events
                .iter()
                .filter_map(|event| event["stage"].as_str().map(ToString::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let failure_classification = record["failure"].as_str().map(ToString::to_string);
    let failure_detail = record["failure_detail"].as_str().map(ToString::to_string);
    let observed_failure = failure_classification.is_some();
    let observed_success = !observed_failure && record["bundle_id"].as_str().is_some();
    let expected_outcome_met = match submission.transaction.expected_outcome {
        ExpectedOutcome::Success => observed_success,
        ExpectedOutcome::Failure => observed_failure,
    };
    let mut notes = Vec::new();
    if record["signature"].as_str() != Some(&submission.transaction.signature) {
        notes.push("signature mismatch".to_string());
    }
    if record["blockhash"].as_str() != Some(&submission.transaction.blockhash) {
        notes.push("blockhash mismatch".to_string());
    }
    if record["tip_lamports"].as_u64() != Some(submission.transaction.tip_lamports) {
        notes.push("tip_lamports mismatch".to_string());
    }
    if slot_numbers.submitted_slot.is_none() {
        notes.push("missing submitted_slot".to_string());
    }
    if commitment_progression.is_empty() {
        notes.push("missing commitment events".to_string());
    }
    if !expected_outcome_met {
        notes.push("expected outcome was not observed".to_string());
    }

    SubmissionValidation {
        matched_lifecycle_record: true,
        observed_success,
        observed_failure,
        expected_outcome_met,
        slot_numbers,
        commitment_progression,
        failure_classification,
        failure_detail,
        notes,
    }
}

fn read_lifecycle_records(path: &Path) -> anyhow::Result<Vec<Value>> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("failed reading lifecycle log {}", path.display()));
        }
    };
    contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<Value>(line).map_err(Into::into))
        .collect()
}

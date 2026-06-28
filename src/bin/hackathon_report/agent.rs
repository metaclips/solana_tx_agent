use std::{
    fs::File,
    process::{Child, Command, Output, Stdio},
    time::Duration,
};

use anyhow::Context;
use chrono::Utc;

use crate::{
    config::ReportConfig,
    types::{AgentRunReport, GeneratedTransaction, SubmissionReport},
    util::{run_with_timeout, sibling_binary},
};

pub(crate) fn start_agent(config: &ReportConfig) -> anyhow::Result<AgentGuard> {
    let stdout = File::create(&config.agent_log_path)
        .with_context(|| format!("failed creating {}", config.agent_log_path.display()))?;
    let stderr = stdout.try_clone()?;
    let mut command = Command::new(sibling_binary("agent_mcp")?);
    command
        .arg("--bind")
        .arg(&config.mcp_bind_addr)
        .env("YELLOWSTONE_ENDPOINT", &config.yellowstone_endpoint)
        .env("SOLANA_RPC_URL", &config.solana_rpc_url)
        .env("JITO_AUTH_KEYPAIR", &config.jito_auth_keypair_path)
        .env("LIFECYCLE_LOG", &config.lifecycle_log_path)
        .env("MCP_BIND_ADDR", &config.mcp_bind_addr)
        .env("MAX_AGENT_RETRIES", "0")
        .env(
            "CONFIRMATION_TIMEOUT_SECS",
            config.confirmation_timeout_secs.to_string(),
        )
        .env(
            "LEADER_LOOKAHEAD_SLOTS",
            config.leader_lookahead_slots.to_string(),
        )
        .env("TIP_FLOOR_LAMPORTS", config.tip_lamports.to_string())
        .env(
            "MAX_AGENT_WAIT_SLOTS",
            config.max_agent_wait_slots.to_string(),
        )
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    if let Some(token) = &config.yellowstone_token {
        command.env("YELLOWSTONE_TOKEN", token);
    }

    Ok(AgentGuard {
        child: command.spawn().context("failed starting agent_mcp")?,
    })
}

pub(crate) fn submit_transaction_with_agent(
    config: &ReportConfig,
    transaction: GeneratedTransaction,
) -> anyhow::Result<SubmissionReport> {
    let started_at = Utc::now();
    let output = run_agent_host(config, &transaction)?;
    let finished_at = Utc::now();
    let duration_ms = finished_at
        .signed_duration_since(started_at)
        .to_std()
        .unwrap_or_default()
        .as_millis();

    Ok(SubmissionReport {
        request_id: transaction.request_id.clone(),
        transaction,
        agent: AgentRunReport {
            started_at,
            finished_at,
            duration_ms,
            exit_code: output.status.code(),
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        },
        lifecycle: None,
        validation: None,
    })
}

fn run_agent_host(config: &ReportConfig, tx: &GeneratedTransaction) -> anyhow::Result<Output> {
    let mut command = Command::new(sibling_binary("agent_host")?);
    command
        .arg("--mcp-url")
        .arg(config.mcp_url())
        .arg("--request-id")
        .arg(&tx.request_id)
        .arg("--encoding")
        .arg(&tx.encoding)
        .arg("--transaction-file")
        .arg(&tx.transaction_file)
        .arg("--observed-tip-lamports")
        .arg(tx.tip_lamports.to_string())
        .env("YELLOWSTONE_ENDPOINT", &config.yellowstone_endpoint)
        .env("SOLANA_RPC_URL", &config.solana_rpc_url)
        .env("JITO_AUTH_KEYPAIR", &config.jito_auth_keypair_path)
        .env("LIFECYCLE_LOG", &config.lifecycle_log_path)
        .env("MCP_SERVER_URL", config.mcp_url())
        .env("MAX_AGENT_RETRIES", "0")
        .env(
            "CONFIRMATION_TIMEOUT_SECS",
            config.confirmation_timeout_secs.to_string(),
        )
        .env(
            "LEADER_LOOKAHEAD_SLOTS",
            config.leader_lookahead_slots.to_string(),
        )
        .env("TIP_FLOOR_LAMPORTS", config.tip_lamports.to_string())
        .env(
            "MAX_AGENT_WAIT_SLOTS",
            config.max_agent_wait_slots.to_string(),
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(token) = &config.yellowstone_token {
        command.env("YELLOWSTONE_TOKEN", token);
    }

    run_with_timeout(command, Duration::from_secs(config.submission_timeout_secs))
        .with_context(|| format!("agent_host timed out for {}", tx.request_id))
}

#[derive(Debug)]
pub(crate) struct AgentGuard {
    child: Child,
}

impl AgentGuard {
    pub(crate) fn try_wait(&mut self) -> anyhow::Result<Option<std::process::ExitStatus>> {
        Ok(self.child.try_wait()?)
    }
}

impl Drop for AgentGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

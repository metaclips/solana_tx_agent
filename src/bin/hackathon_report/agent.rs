use std::{
    fs::File,
    io::{Read, Write},
    process::{Child, Command, Output, Stdio},
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
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
    let log_file = File::create(&config.agent_log_path)
        .with_context(|| format!("failed creating {}", config.agent_log_path.display()))?;
    let log_file = Arc::new(Mutex::new(log_file));
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
            "YELLOWSTONE_CONNECT_TIMEOUT_SECS",
            config.yellowstone_connect_timeout_secs.to_string(),
        )
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

    let mut child = command.spawn().context("failed starting agent_mcp")?;
    let mut log_threads = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        log_threads.push(spawn_log_forwarder(
            stdout,
            log_file.clone(),
            LogStream::Stdout,
        ));
    }
    if let Some(stderr) = child.stderr.take() {
        log_threads.push(spawn_log_forwarder(stderr, log_file, LogStream::Stderr));
    }

    Ok(AgentGuard { child, log_threads })
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
            "YELLOWSTONE_CONNECT_TIMEOUT_SECS",
            config.yellowstone_connect_timeout_secs.to_string(),
        )
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
    log_threads: Vec<JoinHandle<()>>,
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
        for thread in self.log_threads.drain(..) {
            let _ = thread.join();
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum LogStream {
    Stdout,
    Stderr,
}

fn spawn_log_forwarder<R>(
    mut reader: R,
    log_file: Arc<Mutex<File>>,
    stream: LogStream,
) -> JoinHandle<()>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        loop {
            let read = match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => read,
                Err(_) => break,
            };
            let chunk = &buffer[..read];
            if let Ok(mut file) = log_file.lock() {
                let _ = file.write_all(chunk);
                let _ = file.flush();
            }
            match stream {
                LogStream::Stdout => {
                    let _ = std::io::stdout().write_all(chunk);
                    let _ = std::io::stdout().flush();
                }
                LogStream::Stderr => {
                    let _ = std::io::stderr().write_all(chunk);
                    let _ = std::io::stderr().flush();
                }
            }
        }
    })
}

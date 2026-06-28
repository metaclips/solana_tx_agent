use std::{
    env, fs,
    path::PathBuf,
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use solana_sdk::pubkey::Pubkey;

use crate::util::{env_u64, next_arg};

pub(crate) const DEFAULT_BIND_ADDR: &str = "127.0.0.1:18080";
pub(crate) const DEFAULT_JITO_TIP_ADDRESS: &str = "DfXygSm4jCyNCybVYYK6DwvWqjKee8pbDmJGcLWNDXjh";

#[derive(Debug, Clone)]
pub(crate) struct ReportConfig {
    pub(crate) request_prefix: String,
    pub(crate) solana_rpc_url: String,
    pub(crate) yellowstone_endpoint: String,
    pub(crate) yellowstone_token: Option<String>,
    pub(crate) jito_auth_keypair_path: PathBuf,
    pub(crate) payer_keypair_path: PathBuf,
    pub(crate) tip_account: Pubkey,
    pub(crate) tip_lamports: u64,
    pub(crate) success_count: u64,
    pub(crate) failure_count: u64,
    pub(crate) mcp_bind_addr: String,
    pub(crate) work_dir: PathBuf,
    pub(crate) lifecycle_log_path: PathBuf,
    pub(crate) agent_log_path: PathBuf,
    pub(crate) report_path: PathBuf,
    pub(crate) confirmation_timeout_secs: u64,
    pub(crate) leader_lookahead_slots: u64,
    pub(crate) max_agent_wait_slots: u64,
    pub(crate) server_ready_timeout_secs: u64,
    pub(crate) submission_timeout_secs: u64,
}

impl ReportConfig {
    pub(crate) fn from_args() -> anyhow::Result<Self> {
        let now_ms = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
        let mut request_prefix = format!("hackathon-{now_ms}");
        let mut solana_rpc_url = env::var("SOLANA_RPC_URL").unwrap_or_default();
        let mut yellowstone_endpoint = env::var("YELLOWSTONE_ENDPOINT").unwrap_or_default();
        let yellowstone_token = env::var("YELLOWSTONE_TOKEN").ok();
        let mut jito_auth_keypair_path = env::var("JITO_AUTH_KEYPAIR").map(PathBuf::from).ok();
        let mut payer_keypair_path = env::var("TX_AGENT_REAL_PAYER_KEYPAIR")
            .or_else(|_| env::var("PAYER_KEYPAIR"))
            .map(PathBuf::from)
            .ok();
        let mut tip_account = env::var("TX_AGENT_REAL_TIP_ACCOUNT")
            .unwrap_or_else(|_| DEFAULT_JITO_TIP_ADDRESS.to_string());
        let mut tip_lamports = env_u64("TX_AGENT_REAL_TIP_LAMPORTS", 1_000);
        let mut success_count = env_u64("TX_AGENT_REAL_SUCCESS_COUNT", 8);
        let mut failure_count = env_u64("TX_AGENT_REAL_FAILURE_COUNT", 2);
        let mut mcp_bind_addr = env::var("TX_AGENT_REAL_MCP_BIND_ADDR")
            .unwrap_or_else(|_| DEFAULT_BIND_ADDR.to_string());
        let default_work_dir = env::temp_dir().join(format!("tx-agent-hackathon-report-{now_ms}"));
        let mut work_dir = env::var("TX_AGENT_REPORT_WORK_DIR")
            .map(PathBuf::from)
            .unwrap_or(default_work_dir);
        let mut report_path = env::var("TX_AGENT_HACKATHON_REPORT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("hackathon_report.json"));
        let mut confirmation_timeout_secs = env_u64("CONFIRMATION_TIMEOUT_SECS", 45);
        let mut leader_lookahead_slots = env_u64("LEADER_LOOKAHEAD_SLOTS", 3);
        let mut max_agent_wait_slots = env_u64("MAX_AGENT_WAIT_SLOTS", 3);
        let mut server_ready_timeout_secs = env_u64("TX_AGENT_SERVER_READY_TIMEOUT_SECS", 180);
        let mut submission_timeout_secs = env_u64("TX_AGENT_SUBMISSION_TIMEOUT_SECS", 180);

        let mut iter = env::args().skip(1);
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--request-prefix" => request_prefix = next_arg(&mut iter, &arg)?,
                "--rpc-url" => solana_rpc_url = next_arg(&mut iter, &arg)?,
                "--yellowstone-endpoint" => yellowstone_endpoint = next_arg(&mut iter, &arg)?,
                "--jito-auth-keypair" => {
                    jito_auth_keypair_path = Some(PathBuf::from(next_arg(&mut iter, &arg)?));
                }
                "--payer-keypair" => {
                    payer_keypair_path = Some(PathBuf::from(next_arg(&mut iter, &arg)?));
                }
                "--tip-account" => tip_account = next_arg(&mut iter, &arg)?,
                "--tip-lamports" => tip_lamports = next_arg(&mut iter, &arg)?.parse()?,
                "--success-count" => success_count = next_arg(&mut iter, &arg)?.parse()?,
                "--failure-count" => failure_count = next_arg(&mut iter, &arg)?.parse()?,
                "--bind" => mcp_bind_addr = next_arg(&mut iter, &arg)?,
                "--work-dir" => work_dir = PathBuf::from(next_arg(&mut iter, &arg)?),
                "--out" => report_path = PathBuf::from(next_arg(&mut iter, &arg)?),
                "--confirmation-timeout-secs" => {
                    confirmation_timeout_secs = next_arg(&mut iter, &arg)?.parse()?;
                }
                "--leader-lookahead-slots" => {
                    leader_lookahead_slots = next_arg(&mut iter, &arg)?.parse()?;
                }
                "--max-agent-wait-slots" => {
                    max_agent_wait_slots = next_arg(&mut iter, &arg)?.parse()?;
                }
                "--server-ready-timeout-secs" => {
                    server_ready_timeout_secs = next_arg(&mut iter, &arg)?.parse()?;
                }
                "--submission-timeout-secs" => {
                    submission_timeout_secs = next_arg(&mut iter, &arg)?.parse()?;
                }
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                other => bail!("unknown argument {other}; use --help"),
            }
        }

        if solana_rpc_url.is_empty() {
            bail!("set SOLANA_RPC_URL or pass --rpc-url");
        }
        if yellowstone_endpoint.is_empty() {
            bail!("set YELLOWSTONE_ENDPOINT or pass --yellowstone-endpoint");
        }
        if success_count + failure_count != 10 {
            bail!("success-count + failure-count must equal 10");
        }
        if failure_count < 2 {
            bail!("failure-count must be at least 2");
        }

        fs::create_dir_all(&work_dir)?;
        let lifecycle_log_path = work_dir.join("lifecycle.log.jsonl");
        let agent_log_path = work_dir.join("agent_mcp.log");

        Ok(Self {
            request_prefix,
            solana_rpc_url,
            yellowstone_endpoint,
            yellowstone_token,
            jito_auth_keypair_path: jito_auth_keypair_path
                .context("set JITO_AUTH_KEYPAIR or pass --jito-auth-keypair")?,
            payer_keypair_path: payer_keypair_path
                .context("set TX_AGENT_REAL_PAYER_KEYPAIR or pass --payer-keypair")?,
            tip_account: Pubkey::from_str(&tip_account)
                .with_context(|| format!("invalid tip account {tip_account}"))?,
            tip_lamports,
            success_count,
            failure_count,
            mcp_bind_addr,
            work_dir,
            lifecycle_log_path,
            agent_log_path,
            report_path,
            confirmation_timeout_secs,
            leader_lookahead_slots,
            max_agent_wait_slots,
            server_ready_timeout_secs,
            submission_timeout_secs,
        })
    }

    pub(crate) fn mcp_url(&self) -> String {
        format!("http://{}/mcp", self.mcp_bind_addr)
    }
}

fn print_help() {
    println!(
        "Usage: cargo run --bin hackathon_report -- [options]\n\n\
Required via args or env:\n  \
--rpc-url URL                      or SOLANA_RPC_URL\n  \
--yellowstone-endpoint URL         or YELLOWSTONE_ENDPOINT\n  \
--jito-auth-keypair PATH           or JITO_AUTH_KEYPAIR\n  \
--payer-keypair PATH               or TX_AGENT_REAL_PAYER_KEYPAIR\n\n\
Common options:\n  \
--out PATH                         report JSON path (default hackathon_report.json)\n  \
--work-dir PATH                    generated tx and lifecycle log directory\n  \
--request-prefix ID                submission id prefix\n  \
--success-count N                  default 8\n  \
--failure-count N                  default 2, must be at least 2\n  \
--tip-lamports N                   default 1000\n  \
--tip-account PUBKEY               default Jito tip account\n  \
--bind HOST:PORT                   default 127.0.0.1:18080"
    );
}

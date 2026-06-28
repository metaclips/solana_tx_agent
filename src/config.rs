use std::{env, path::PathBuf, time::Duration};

use anyhow::Context;
use solana_sdk::{pubkey::Pubkey, signature::Keypair, signer::Signer};

#[derive(Debug, Clone)]
pub enum Mode {
    Run,
    Submit { count: usize },
    FaultExpiredBlockhash,
    PrintLog,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub mode: Mode,
    pub yellowstone_endpoint: String,
    pub yellowstone_token: Option<String>,
    pub solana_rpc_url: String,
    pub payer_keypair_path: Option<PathBuf>,
    pub jito_auth_keypair_path: Option<PathBuf>,
    pub jito_block_engine_url: String,
    pub lifecycle_log_path: PathBuf,
    pub submit_count: usize,
    pub leader_lookahead_slots: u64,
    pub tip_floor_lamports: u64,
    pub self_transfer_lamports: u64,
    pub confirmation_timeout: Duration,
    pub openai_api_key: Option<String>,
    pub openai_model: String,
}

impl Config {
    pub fn from_env_and_args() -> anyhow::Result<Self> {
        let args = env::args().skip(1).collect::<Vec<_>>();
        let mode = parse_mode(&args)?;

        let submit_count = match mode {
            Mode::Submit { count } => count,
            _ => env_usize("SUBMIT_COUNT", 1),
        };

        Ok(Self {
            mode,
            yellowstone_endpoint: env::var("YELLOWSTONE_ENDPOINT")
                .unwrap_or_else(|_| "https://yellowstone.eu.fluxrpc.com/".to_string()),
            yellowstone_token: env::var("YELLOWSTONE_TOKEN").ok().filter(|v| !v.is_empty()),
            solana_rpc_url: env::var("SOLANA_RPC_URL")
                .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string()),
            payer_keypair_path: env::var("PAYER_KEYPAIR")
                .ok()
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            jito_auth_keypair_path: env::var("JITO_AUTH_KEYPAIR")
                .ok()
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            jito_block_engine_url: env::var("JITO_BLOCK_ENGINE_URL")
                .unwrap_or_else(|_| "https://frankfurt.mainnet.block-engine.jito.wtf".to_string()),
            lifecycle_log_path: env::var("LIFECYCLE_LOG")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("lifecycle.log.jsonl")),
            submit_count,
            leader_lookahead_slots: env_u64("LEADER_LOOKAHEAD_SLOTS", 3),
            tip_floor_lamports: env_u64("TIP_FLOOR_LAMPORTS", 1_000),
            self_transfer_lamports: env_u64("SELF_TRANSFER_LAMPORTS", 1),
            confirmation_timeout: Duration::from_secs(env_u64("CONFIRMATION_TIMEOUT_SECS", 90)),
            openai_api_key: env::var("OPENAI_API_KEY").ok().filter(|v| !v.is_empty()),
            openai_model: env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4.1-mini".to_string()),
        })
    }

    pub fn payer_keypair(&self) -> anyhow::Result<Keypair> {
        let path = self
            .payer_keypair_path
            .as_ref()
            .context("PAYER_KEYPAIR is required for run/submit/fault modes")?;
        solana_sdk::signature::read_keypair_file(path).map_err(|err| {
            anyhow::anyhow!("failed reading payer keypair {}: {err}", path.display())
        })
    }

    pub fn jito_auth_keypair(&self) -> anyhow::Result<Keypair> {
        if let Some(path) = &self.jito_auth_keypair_path {
            solana_sdk::signature::read_keypair_file(path).map_err(|err| {
                anyhow::anyhow!("failed reading Jito auth keypair {}: {err}", path.display())
            })
        } else {
            let payer = self.payer_keypair()?;
            tracing::warn!(
                "JITO_AUTH_KEYPAIR is not set; using payer {} for Jito auth",
                payer.pubkey()
            );
            Ok(payer)
        }
    }

    pub fn payer_pubkey(&self) -> anyhow::Result<Pubkey> {
        Ok(self.payer_keypair()?.pubkey())
    }
}

fn parse_mode(args: &[String]) -> anyhow::Result<Mode> {
    match args.first().map(String::as_str) {
        None | Some("run") => Ok(Mode::Run),
        Some("submit") => {
            let mut count = env_usize("SUBMIT_COUNT", 10);
            let mut iter = args.iter().skip(1);
            while let Some(arg) = iter.next() {
                match arg.as_str() {
                    "--count" | "-n" => {
                        let raw = iter.next().context("--count requires a value")?;
                        count = raw.parse().context("invalid --count value")?;
                    }
                    other => anyhow::bail!("unknown submit argument: {other}"),
                }
            }
            Ok(Mode::Submit { count })
        }
        Some("fault") => match args.get(1).map(String::as_str) {
            Some("expired-blockhash") => Ok(Mode::FaultExpiredBlockhash),
            _ => anyhow::bail!("usage: tx_agent fault expired-blockhash"),
        },
        Some("print-log") => Ok(Mode::PrintLog),
        Some(other) => anyhow::bail!(
            "unknown command {other}; use run, submit --count N, fault expired-blockhash, or print-log"
        ),
    }
}

fn env_u64(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
}

fn env_usize(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
}

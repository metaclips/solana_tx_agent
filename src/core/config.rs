use std::{env, path::PathBuf, time::Duration};

use solana_sdk::signature::Keypair;

#[derive(Debug, Clone)]
pub struct Config {
    pub yellowstone_endpoint: String,
    pub yellowstone_token: Option<String>,
    pub solana_rpc_url: String,
    pub jito_auth_keypair_path: Option<PathBuf>,
    pub jito_block_engine_url: String,
    pub lifecycle_log_path: PathBuf,
    pub leader_lookahead_slots: u64,
    pub tip_floor_lamports: u64,
    pub confirmation_timeout: Duration,
    pub openai_api_key: Option<String>,
    pub openai_model: String,
    pub max_agent_tip_lamports: u64,
    pub max_agent_retries: u32,
    pub max_agent_wait_slots: u64,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            yellowstone_endpoint: env::var("YELLOWSTONE_ENDPOINT")
                .unwrap_or_else(|_| "https://yellowstone.eu.fluxrpc.com/".to_string()),
            yellowstone_token: env::var("YELLOWSTONE_TOKEN").ok().filter(|v| !v.is_empty()),
            solana_rpc_url: env::var("SOLANA_RPC_URL")
                .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string()),
            jito_auth_keypair_path: env::var("JITO_AUTH_KEYPAIR")
                .ok()
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            jito_block_engine_url: env::var("JITO_BLOCK_ENGINE_URL")
                .unwrap_or_else(|_| "https://frankfurt.mainnet.block-engine.jito.wtf".to_string()),
            lifecycle_log_path: env::var("LIFECYCLE_LOG")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("lifecycle.log.jsonl")),
            leader_lookahead_slots: env_u64("LEADER_LOOKAHEAD_SLOTS", 3),
            tip_floor_lamports: env_u64("TIP_FLOOR_LAMPORTS", 1_000),
            confirmation_timeout: Duration::from_secs(env_u64("CONFIRMATION_TIMEOUT_SECS", 90)),
            openai_api_key: env::var("OPENAI_API_KEY").ok().filter(|v| !v.is_empty()),
            openai_model: env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4.1-mini".to_string()),
            max_agent_tip_lamports: env_u64("MAX_AGENT_TIP_LAMPORTS", 1_000_000),
            max_agent_retries: env_u64("MAX_AGENT_RETRIES", 2) as u32,
            max_agent_wait_slots: env_u64("MAX_AGENT_WAIT_SLOTS", 8),
        })
    }

    pub fn jito_auth_keypair(&self) -> anyhow::Result<Keypair> {
        if let Some(path) = &self.jito_auth_keypair_path {
            solana_sdk::signature::read_keypair_file(path).map_err(|err| {
                anyhow::anyhow!("failed reading Jito auth keypair {}: {err}", path.display())
            })
        } else {
            anyhow::bail!("JITO_AUTH_KEYPAIR is required for Jito bundle submission")
        }
    }
}

fn env_u64(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
}

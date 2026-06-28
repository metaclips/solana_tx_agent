use std::{net::SocketAddr, sync::Arc};

use crate::core::{
    config::Config,
    lifecycle::{AgentAuditRecord, FailureKind},
    stack::{ControlledSignedSubmitRequest, SignedTransactionEncoding, TxStack},
};
use axum::Router;
use chrono::Utc;
use rmcp::{
    Json, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing_subscriber::{EnvFilter, fmt};

#[derive(Clone)]
struct SolanaControlPlane {
    stack: std::sync::Arc<TxStack>,
    tool_router: ToolRouter<Self>,
}

impl SolanaControlPlane {
    fn with_stack(stack: Arc<TxStack>) -> Self {
        Self {
            stack,
            tool_router: Self::tool_router(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
struct EmptyParams {}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
struct ClassifyFailureParams {
    detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum SignedTransactionEncodingParam {
    Base64,
    Base58,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
struct SubmitSignedBundleParams {
    submission_id: String,
    attempt: u32,
    encoded_transaction: String,
    encoding: SignedTransactionEncodingParam,
    wait_for_leader: bool,
    max_wait_slots: Option<u64>,
    observed_tip_lamports: Option<u64>,
}

impl From<SignedTransactionEncodingParam> for SignedTransactionEncoding {
    fn from(value: SignedTransactionEncodingParam) -> Self {
        match value {
            SignedTransactionEncodingParam::Base64 => Self::Base64,
            SignedTransactionEncodingParam::Base58 => Self::Base58,
        }
    }
}

impl From<SubmitSignedBundleParams> for ControlledSignedSubmitRequest {
    fn from(value: SubmitSignedBundleParams) -> Self {
        Self {
            submission_id: value.submission_id,
            attempt: value.attempt,
            encoded_transaction: value.encoded_transaction,
            encoding: value.encoding.into(),
            wait_for_leader: value.wait_for_leader,
            max_wait_slots: value.max_wait_slots,
            observed_tip_lamports: value.observed_tip_lamports,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
struct RecordAgentDecisionParams {
    request_id: String,
    attempt: u32,
    input_state: serde_json::Value,
    decision: serde_json::Value,
    policy: serde_json::Value,
    outcome: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
struct ClassifyFailureOutput {
    failure: FailureKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
struct SubmitSignedBundleOutput {
    record: crate::core::lifecycle::LifecycleRecord,
    failure_report: Option<crate::core::stack::FailureReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
struct RecordAgentDecisionOutput {
    ok: bool,
}

impl From<RecordAgentDecisionParams> for AgentAuditRecord {
    fn from(value: RecordAgentDecisionParams) -> Self {
        Self {
            request_id: value.request_id,
            attempt: value.attempt,
            at: Utc::now(),
            input_state: value.input_state,
            decision: value.decision,
            policy: value.policy,
            outcome: value.outcome,
        }
    }
}

#[tool_router]
impl SolanaControlPlane {
    #[tool(
        description = "Return current slot, Jito leader window, latest blockhash slot, live Jito tip data, and policy tip limits."
    )]
    async fn get_network_state(
        &self,
        _params: Parameters<EmptyParams>,
    ) -> Result<Json<crate::core::stack::NetworkState>, String> {
        Ok(Json(self.stack.operational_state().await))
    }

    #[tool(description = "Return recent Jito tip percentile data only.")]
    async fn get_recent_tip_data(
        &self,
        _params: Parameters<EmptyParams>,
    ) -> Result<Json<crate::core::jito::tip::TipData>, String> {
        Ok(Json(self.stack.operational_state().await.recent_tip))
    }

    #[tool(
        description = "Classify a raw Solana/Jito failure string into the stack failure taxonomy."
    )]
    async fn classify_failure(
        &self,
        Parameters(params): Parameters<ClassifyFailureParams>,
    ) -> Result<Json<ClassifyFailureOutput>, String> {
        Ok(Json(ClassifyFailureOutput {
            failure: FailureKind::classify(&params.detail),
        }))
    }

    #[tool(
        description = "Controlled write tool. Server verifies an encoded pre-signed Solana transaction, submits it to Jito as a bundle, tracks lifecycle, classifies failures, and logs the outcome. The server does not mutate or sign the transaction."
    )]
    async fn submit_signed_bundle(
        &self,
        Parameters(params): Parameters<SubmitSignedBundleParams>,
    ) -> Result<Json<SubmitSignedBundleOutput>, String> {
        let record = self
            .stack
            .submit_signed_encoded(params.into())
            .await
            .map_err(|err| err.to_string())?;
        self.stack
            .log_lifecycle(&record)
            .await
            .map_err(|err| err.to_string())?;
        let failure_report = TxStack::failure_report(&record);
        Ok(Json(SubmitSignedBundleOutput {
            record,
            failure_report,
        }))
    }

    #[tool(
        description = "Append an auditable agent decision record with input state, selected action, policy validation, and final outcome."
    )]
    async fn record_agent_decision(
        &self,
        Parameters(params): Parameters<RecordAgentDecisionParams>,
    ) -> Result<Json<RecordAgentDecisionOutput>, String> {
        self.stack
            .log_agent_audit(&params.into())
            .await
            .map_err(|err| err.to_string())?;
        Ok(Json(RecordAgentDecisionOutput { ok: true }))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for SolanaControlPlane {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(
                "Solana transaction control plane. The server owns RPC, Yellowstone, Jito access, bundle submission, lifecycle tracking, and failure classification. The flow is submit_signed_bundle with a pre-signed encoded transaction; the server verifies and submits it without mutating or signing it. Clients should use tools only for bounded operational decisions.",
            )
    }
}

pub async fn run() -> anyhow::Result<()> {
    fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let bind_addr = parse_bind_addr()?;
    let config = Config::from_env()?;
    let stack = Arc::new(TxStack::connect(config).await?);
    let ready_slot = stack.wait_for_next_slot().await?;

    let service = StreamableHttpService::new(
        {
            let stack = stack.clone();
            move || Ok(SolanaControlPlane::with_stack(stack.clone()))
        },
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default().disable_allowed_hosts(),
    );
    let router = Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;

    tracing::info!(
        "Solana MCP control plane running: {}",
        json!({
            "transport": "streamable_http",
            "sdk": "rmcp",
            "bind_addr": bind_addr.to_string(),
            "url": format!("http://{bind_addr}/mcp"),
            "ready_slot": ready_slot
        })
    );
    axum::serve(listener, router).await?;
    Ok(())
}

fn parse_bind_addr() -> anyhow::Result<SocketAddr> {
    let mut bind = std::env::var("MCP_BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_string());
    let mut iter = std::env::args().skip(1);

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--bind" => {
                bind = iter
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--bind requires a value"))?;
            }
            other => anyhow::bail!("unknown agent_mcp argument: {other}"),
        }
    }

    bind.parse()
        .map_err(|err| anyhow::anyhow!("invalid MCP bind address {bind}: {err}"))
}

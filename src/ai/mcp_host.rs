use std::{path::PathBuf, time::Duration};

use crate::{
    ai::{
        agent::{AgentFailureReport, AiAgent, OperationalAction, OperationalContext},
        policy::{PolicyLimits, ValidatedDecision, validate_decision},
    },
    core::{
        config::Config,
        lifecycle::{AgentAuditRecord, LifecycleRecord},
        stack::{FailureReport, NetworkState},
    },
};
use anyhow::Context;
use chrono::Utc;
use rmcp::{
    ServiceExt,
    model::{CallToolRequestParams, CallToolResult},
    transport::StreamableHttpClientTransport,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};
use tracing::{info, warn};
use tracing_subscriber::{EnvFilter, fmt};

#[derive(Debug, Deserialize, Serialize)]
struct SubmitToolResponse {
    record: LifecycleRecord,
    failure_report: Option<FailureReport>,
}

#[derive(Debug)]
struct HostArgs {
    request_id: String,
    mcp_url: String,
    encoded_transaction: String,
    encoding: SignedTransactionEncodingArg,
    observed_tip_lamports: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum SignedTransactionEncodingArg {
    Base64,
    Base58,
}

pub async fn run() -> anyhow::Result<()> {
    fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let args = parse_args()?;
    let config = Config::from_env()?;
    let agent = AiAgent::new(&config);
    let limits = PolicyLimits::from_config(&config);

    let transport = StreamableHttpClientTransport::from_uri(args.mcp_url.clone());
    let mut client = ().serve(transport).await?;

    let mut attempt = 0u32;
    let mut previous_tip_lamports = args.observed_tip_lamports.unwrap_or(0);
    let mut failure: Option<FailureReport> = None;

    loop {
        let state: NetworkState = call_tool(&mut client, "get_network_state", json!({})).await?;
        let context = OperationalContext {
            request_id: args.request_id.clone(),
            attempt,
            current_slot: Some(state.current_slot),
            leader_slot: state.next_jito_leader_slot,
            slots_until_leader: state.slots_until_jito_leader,
            recent_tip: state.recent_tip.clone(),
            previous_tip_lamports,
            tip_floor_lamports: state.tip_floor_lamports,
            max_tip_lamports: state.max_tip_lamports,
            can_refresh_blockhash: false,
            can_adjust_tip: false,
            failure: failure.as_ref().map(agent_failure_report),
        };

        let decision = agent.decide_operation(&context).await;
        let validated = match validate_decision(decision.clone(), attempt, &limits) {
            Ok(validated) => validated,
            Err(err) => {
                let outcome = json!({ "policy_error": err.to_string() });
                record_decision(
                    &mut client,
                    &args.request_id,
                    attempt,
                    &context,
                    &decision,
                    &limits,
                    Some(outcome),
                )
                .await?;
                anyhow::bail!("agent decision rejected by policy: {err}");
            }
        };

        match validated.decision.action {
            OperationalAction::WaitForLeader => {
                info!("agent chose wait: {}", validated.decision.reason);
                record_decision(
                    &mut client,
                    &args.request_id,
                    attempt,
                    &context,
                    &validated.decision,
                    &limits,
                    Some(json!({"waited": true})),
                )
                .await?;
                tokio::time::sleep(Duration::from_millis(
                    validated.decision.max_wait_slots.max(1) * 400,
                ))
                .await;
                continue;
            }
            OperationalAction::Abandon | OperationalAction::Escalate => {
                info!("agent stopped: {}", validated.decision.reason);
                record_decision(
                    &mut client,
                    &args.request_id,
                    attempt,
                    &context,
                    &validated.decision,
                    &limits,
                    Some(json!({"stopped": true})),
                )
                .await?;
                client.close().await?;
                return Ok(());
            }
            OperationalAction::SubmitNow | OperationalAction::Retry => {
                let response = submit_via_mcp(&mut client, &args, attempt, &validated).await?;

                previous_tip_lamports = response.record.tip_lamports;
                let outcome = serde_json::to_value(&response)?;
                record_decision(
                    &mut client,
                    &args.request_id,
                    attempt,
                    &context,
                    &validated.decision,
                    &limits,
                    Some(outcome),
                )
                .await?;

                if let Some(report) = response.failure_report {
                    warn!(
                        "submission {} attempt {} failed as {:?}: {}",
                        args.request_id, attempt, report.failure, report.detail
                    );
                    failure = Some(report);
                    attempt += 1;
                    if attempt > limits.max_retries {
                        warn!("max retries reached for {}", args.request_id);
                        client.close().await?;
                        return Ok(());
                    }
                    continue;
                }

                info!(
                    "submission {} completed without classified failure",
                    args.request_id
                );
                client.close().await?;
                return Ok(());
            }
        }
    }
}

async fn submit_via_mcp<S>(
    client: &mut rmcp::service::RunningService<rmcp::RoleClient, S>,
    args: &HostArgs,
    attempt: u32,
    validated: &ValidatedDecision,
) -> anyhow::Result<SubmitToolResponse>
where
    S: rmcp::Service<rmcp::RoleClient>,
{
    let request = json!({
        "submission_id": args.request_id.clone(),
        "attempt": attempt,
        "encoded_transaction": args.encoded_transaction.clone(),
        "encoding": args.encoding.clone(),
        "wait_for_leader": matches!(validated.decision.action, OperationalAction::Retry),
        "max_wait_slots": validated.decision.max_wait_slots,
        "observed_tip_lamports": args.observed_tip_lamports.unwrap_or(validated.decision.tip_lamports),
    });

    call_tool(client, "submit_signed_bundle", request).await
}

async fn record_decision<S>(
    client: &mut rmcp::service::RunningService<rmcp::RoleClient, S>,
    request_id: &str,
    attempt: u32,
    context: &OperationalContext,
    decision: &crate::ai::agent::OperationalDecision,
    limits: &PolicyLimits,
    outcome: Option<Value>,
) -> anyhow::Result<()>
where
    S: rmcp::Service<rmcp::RoleClient>,
{
    let record = AgentAuditRecord {
        request_id: request_id.to_string(),
        attempt,
        at: Utc::now(),
        input_state: serde_json::to_value(context)?,
        decision: serde_json::to_value(decision)?,
        policy: serde_json::to_value(limits)?,
        outcome,
    };
    let _: Value = call_tool(
        client,
        "record_agent_decision",
        serde_json::to_value(record)?,
    )
    .await?;
    Ok(())
}

async fn call_tool<T, S>(
    client: &mut rmcp::service::RunningService<rmcp::RoleClient, S>,
    name: &'static str,
    arguments: Value,
) -> anyhow::Result<T>
where
    T: DeserializeOwned,
    S: rmcp::Service<rmcp::RoleClient>,
{
    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new(name).with_arguments(value_to_object(arguments)?))
        .await?;
    decode_tool_result(result)
}

fn decode_tool_result<T: DeserializeOwned>(result: CallToolResult) -> anyhow::Result<T> {
    if result.is_error == Some(true) {
        let text = result
            .content
            .first()
            .and_then(|content| content.as_text())
            .map(|text| text.text.clone())
            .unwrap_or_else(|| "tool returned an error".into());
        anyhow::bail!("{text}");
    }
    let value = if let Some(value) = result.structured_content {
        value
    } else {
        let text = result
            .content
            .first()
            .and_then(|content| content.as_text())
            .context("tool response missing structured content and text content")?;
        serde_json::from_str(&text.text)?
    };
    Ok(serde_json::from_value(value)?)
}

fn value_to_object(value: Value) -> anyhow::Result<Map<String, Value>> {
    match value {
        Value::Object(map) => Ok(map),
        _ => anyhow::bail!("tool arguments must be a JSON object"),
    }
}

fn agent_failure_report(report: &FailureReport) -> AgentFailureReport {
    AgentFailureReport {
        failure: report.failure.clone(),
        detail: report.detail.clone(),
        submitted_slot: report.submitted_slot,
        blockhash: report.blockhash.clone(),
        processed_latency_ms: report.processed_latency_ms,
        confirmed_latency_ms: report.confirmed_latency_ms,
    }
}

fn parse_args() -> anyhow::Result<HostArgs> {
    let mut request_id = format!("agent-request-{}", Utc::now().timestamp_millis());
    let mut mcp_url =
        std::env::var("MCP_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080/mcp".to_string());
    let mut encoded_transaction = None;
    let mut transaction_file: Option<PathBuf> = None;
    let mut encoding = SignedTransactionEncodingArg::Base64;
    let mut observed_tip_lamports = None;
    let mut iter = std::env::args().skip(1);

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--request-id" => {
                request_id = iter.next().context("--request-id requires a value")?;
            }
            "--mcp-url" => {
                mcp_url = iter.next().context("--mcp-url requires a value")?;
            }
            "--encoded-transaction" => {
                encoded_transaction = Some(
                    iter.next()
                        .context("--encoded-transaction requires a value")?,
                );
            }
            "--transaction-file" => {
                let path = iter.next().context("--transaction-file requires a value")?;
                transaction_file = Some(PathBuf::from(path));
            }
            "--encoding" => {
                encoding = parse_encoding(&iter.next().context("--encoding requires a value")?)?;
            }
            "--observed-tip-lamports" => {
                observed_tip_lamports = Some(
                    iter.next()
                        .context("--observed-tip-lamports requires a value")?
                        .parse()
                        .context("invalid --observed-tip-lamports value")?,
                );
            }
            other => anyhow::bail!("unknown agent_host argument: {other}"),
        }
    }

    let encoded_transaction = match (encoded_transaction, transaction_file) {
        (Some(tx), None) => tx,
        (None, Some(path)) => std::fs::read_to_string(&path)
            .with_context(|| format!("failed reading transaction file {}", path.display()))?
            .trim()
            .to_string(),
        (Some(_), Some(_)) => {
            anyhow::bail!("use either --encoded-transaction or --transaction-file, not both")
        }
        (None, None) => anyhow::bail!(
            "agent_host requires a pre-signed encoded transaction; pass --encoded-transaction VALUE or --transaction-file PATH"
        ),
    };

    Ok(HostArgs {
        request_id,
        mcp_url,
        encoded_transaction,
        encoding,
        observed_tip_lamports,
    })
}

fn parse_encoding(value: &str) -> anyhow::Result<SignedTransactionEncodingArg> {
    match value {
        "base64" => Ok(SignedTransactionEncodingArg::Base64),
        "base58" => Ok(SignedTransactionEncodingArg::Base58),
        other => anyhow::bail!("unsupported --encoding {other}; use base64 or base58"),
    }
}

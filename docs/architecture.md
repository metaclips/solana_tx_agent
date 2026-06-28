# tx_agent Architecture

This document is the source draft for the public architecture document. Publish this content to Notion, Google Docs, Figma, or GitHub Pages and include the public URL in the final submission.

## System Overview

`tx_agent` is an MCP-based transaction control plane for Solana. It observes network state through Yellowstone/Geyser, times submissions around connected Jito leaders, sends real Jito bundles, tracks lifecycle progression, classifies failures, and delegates one bounded operational decision to a client-side decision host.

The core design principle is separation of authority:

- MCP server owns infrastructure access, Jito access, bundle submission, lifecycle tracking, and failure classification.
- MCP client hosts the operational decision loop and receives already signed encoded transactions.
- Local policy validates decision output before any write tool executes.
- Decisions and outcomes are logged for audit.

The MCP server and client are implemented with the official `modelcontextprotocol/rust-sdk` Rust crate, published as `rmcp`. The server uses MCP streamable HTTP at `/mcp`; the client host connects with `rmcp::transport::StreamableHttpClientTransport`.

## Components

### Config

Module: `src/core/config.rs`

Reads RPC, Yellowstone, Jito auth, logging, tip, and policy settings from environment variables.

### MCP Server

Implementation: `src/core/mcp/server.rs`

Launcher: `src/bin/agent_mcp.rs`

Responsibilities:

- Own all Solana RPC, Yellowstone, Jito auth, and bundle-submission access.
- Run as a long-lived MCP streamable HTTP service.
- Maintain live slot/blockhash streams.
- Track connected Jito leader opportunities.
- Expose live network state and recent tip data.
- Verify pre-signed encoded transactions and submit bundles through a narrow controlled write tool.
- Track lifecycle progression.
- Classify failures.
- Append lifecycle and decision audit logs.

Tools:

- `get_network_state`
- `get_recent_tip_data`
- `classify_failure`
- `submit_signed_bundle`
- `record_agent_decision`

The `submit_signed_bundle` tool accepts a submission ID, attempt number, encoded signed transaction, encoding, leader-wait flag, max wait slots, and observed tip metadata. It verifies and submits the signed transaction unchanged.

### MCP Client / Decision Host

Implementation: `src/ai/mcp_host.rs`

Launcher: `src/bin/agent_host.rs`

Responsibilities:

- Receive an already signed encoded transaction request.
- Connect to an already running MCP server over HTTP.
- Query MCP tools for live state.
- Build the minimal context needed for a structured operational decision.
- Select an action for the signed payload.
- Validate that decision locally against policy limits.
- Execute only allowed MCP write tools.
- Record every input, decision, policy, and outcome.

### Yellowstone/Geyser Stream

Module: `src/core/networking/geyser.rs`

Responsibilities:

- Stream slots and block metadata.
- Maintain current slot and latest streamed blockhash.
- Subscribe to exact transaction signature status using `transactions_status`.
- Reconnect with exponential backoff.

### Jito Client

Module: `src/core/jito/client.rs`

Responsibilities:

- Authenticate with the Jito block engine.
- Subscribe to bundle result stream.
- Fetch connected Jito leaders.
- Fetch Jito tip accounts.
- Ingest live tip-floor data.
- Submit bundles and handle rate-limit retries.
- Broadcast bundle lifecycle events to the stack.

### Lifecycle Tracker

Module: `src/core/lifecycle.rs`

Records:

- Bundle ID and signature.
- Submitted, accepted, processed, confirmed, finalized events.
- Timestamps and slots.
- Latency deltas.
- Failure classification.

The output is JSONL for simple auditability.

### Decision Engine

Module: `src/ai/agent/mod.rs`

Decision owned by the client-side host: when and how to submit or retry.

Input evidence:

- Request ID and attempt number.
- Current slot and nearest Jito leader window.
- Recent Jito tip percentiles.
- Previous tip.
- Policy tip/retry/wait limits.
- Failure kind and raw details.
- Whether the already signed transaction can be refreshed or tip-adjusted. In this flow those capabilities are false.

Output:

- `submit_now`, `wait_for_leader`, `retry`, `abandon`, or `escalate`.
- Whether to refresh blockhash. For already signed transactions this is forced false by policy/context.
- Desired or observed tip amount for reasoning and audit.
- Max wait slots.
- Reasoning string.

The implementation supports a deterministic fallback path and validates structured decision output before any write operation.

### Policy Engine

Module: `src/ai/policy.rs`

The policy engine validates decision output before any write tool executes:

- Tip must be within `TIP_FLOOR_LAMPORTS` and `MAX_AGENT_TIP_LAMPORTS`.
- Retry/write attempts must not exceed `MAX_AGENT_RETRIES`.
- Wait slots are capped by `MAX_AGENT_WAIT_SLOTS`.
- Invalid decisions are rejected and logged.

## Data Flow

1. `agent_host` receives an already signed encoded transaction request.
2. `agent_host` connects to the already running `agent_mcp` HTTP MCP endpoint.
3. `agent_mcp` has already started `TxStack`.
4. `TxStack` maintains Yellowstone slot/blockhash streaming.
5. Jito client authenticates, subscribes to bundle results, fetches leaders, fetches tip accounts, and starts tip stream.
6. `agent_host` calls `get_network_state`.
7. `agent_host` builds an `OperationalContext` and selects a structured decision.
8. `agent_host` validates the decision through the policy engine.
9. If the decision is `submit_now` or `retry`, `agent_host` calls `submit_signed_bundle`.
10. `agent_mcp` verifies the signed transaction, optionally waits for the configured leader window, submits the Jito bundle, subscribes to signature status, and tracks lifecycle.
11. Jito bundle events and Yellowstone signature events update the lifecycle record.
12. RPC commitment polling fills in confirmed/finalized fallback evidence.
13. On failure, `agent_mcp` returns a structured failure report.
14. `agent_host` records the decision and outcome through `record_agent_decision`.
15. If failure is retryable and policy allows it, the decision layer chooses whether to retry, wait, abandon, or escalate.

## Failure Handling

Classified failures:

- Expired blockhash
- Fee/tip too low
- Compute exceeded
- Jito bundle failure
- Jito rate limit
- Simulation failure
- Timeout
- Unknown

Expired blockhash is handled by returning a structured failure report to the client host. The decision layer cannot refresh the blockhash because doing so would invalidate the signature; it escalates for a newly signed transaction.

Fee/tip failures are similar. The decision layer cannot mutate the embedded tip, so it escalates for a newly signed transaction with a higher tip.

Compute and simulation failures default to abandon because blockhash/tip changes do not fix transaction construction errors.

## Infrastructure Decisions

- Yellowstone is primary for slot and signature lifecycle evidence.
- RPC is used for commitment fallback.
- Jito connected leader data is used before submission to avoid blind sending.
- Lifecycle evidence is append-only JSONL for auditability.
- Decision-host records are written to `agent_decisions.log.jsonl`.

## Suggested Diagram

```text
          +---------------------+
          |  MCP Client         |
          |  agent_host         |
          +----+-----------+----+
               |           |
               v           v
        +----------+   +-------------+
        | Decision |   | Policy      |
        | Engine   |   | Validator   |
        +-----+----+   +------+------+
              |               |
              +-------+-------+
                      |
                      v
          +---------------------+
          | MCP Server          |
          | agent_mcp           |
          +----+------------+---+
               |            |
               v            v
       +--------------+   +----------------+
       | Yellowstone  |   | Jito Client    |
       | Slot/Status  |   | Leaders/Bundle |
       +------+-------+   +---+------------+
              |               |
              v               v
        +-----------+   +-------------+
        | Lifecycle |<--| Bundle Feed |
        | Logger    |   +-------------+
        +-----------+
```

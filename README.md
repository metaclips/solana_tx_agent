# tx_agent

Smart Solana transaction infrastructure prototype for the hackathon bounty. It combines Yellowstone/Geyser slot and signature-status streams, Jito bundle submission, live Jito tip data, lifecycle logging, and an AI-owned operational decision.

The project is MCP-based:

- `agent_mcp` is the infrastructure control plane. It owns RPC, Yellowstone, Jito auth, bundle submission, lifecycle tracking, and failure classification.
- `agent_host` is the MCP client and agent host. It receives an already signed encoded transaction, queries MCP tools for live state, asks the LLM for one operational decision, validates that decision locally, and only then calls MCP write tools.
- The agent never signs transactions, mutates transaction fields, sees private keys, or directly calls Solana/Jito. If the signed transaction needs a fresh blockhash or higher embedded tip, the agent escalates for a new signed transaction.

The MCP implementation uses the official [`modelcontextprotocol/rust-sdk`](https://github.com/modelcontextprotocol/rust-sdk), published as the `rmcp` crate.

## What It Does

- Streams live slots and blockhashes from Yellowstone/Geyser.
- Fetches Jito connected leader windows and waits for a near leader before submission.
- Accepts a pre-signed base64 or base58 Solana transaction from the agent host.
- Verifies and submits the signed transaction as a Jito bundle.
- Tracks bundle events from Jito and signature status from Yellowstone.
- Uses RPC commitment checks as fallback/corroboration, not as the only lifecycle source.
- Logs submitted, accepted, processed, confirmed, finalized, failure, latency, slot, and tip data to JSONL.
- Lets the AI agent decide submit, wait, retry, abandon, or escalate after failure.
- Exposes controlled MCP tools for live state, failure classification, bundle submission, and agent-decision audit logging.

## Setup

Required environment:

```sh
export YELLOWSTONE_ENDPOINT="https://your-yellowstone-endpoint"
export YELLOWSTONE_TOKEN="optional-token"
export SOLANA_RPC_URL="https://your-rpc"
export JITO_AUTH_KEYPAIR="$HOME/.config/solana/jito-auth.json"
```

Optional environment:

```sh
export JITO_BLOCK_ENGINE_URL="https://frankfurt.mainnet.block-engine.jito.wtf"
export LIFECYCLE_LOG="lifecycle.log.jsonl"
export LEADER_LOOKAHEAD_SLOTS=3
export TIP_FLOOR_LAMPORTS=1000
export CONFIRMATION_TIMEOUT_SECS=90
export OPENAI_API_KEY="optional"
export OPENAI_MODEL="gpt-4.1-mini"
export MAX_AGENT_TIP_LAMPORTS=1000000
export MAX_AGENT_RETRIES=2
export MAX_AGENT_WAIT_SLOTS=8
export MCP_BIND_ADDR="127.0.0.1:8080"
export MCP_SERVER_URL="http://127.0.0.1:8080/mcp"
```

The upstream signer must produce a funded, signed transaction that already contains any desired Jito tip instruction. This project does not accept `PAYER_KEYPAIR` and does not sign or mutate transactions.

## Commands

MCP control-plane demo:

```sh
cargo run --bin agent_mcp -- --bind 127.0.0.1:8080
```

In another shell:

```sh
cargo run --bin agent_host -- \
  --mcp-url http://127.0.0.1:8080/mcp \
  --request-id demo-001 \
  --encoding base64 \
  --transaction-file signed-tx.base64 \
  --observed-tip-lamports 1000
```

`agent_mcp` is a long-running MCP streamable HTTP server at `/mcp`. `agent_host` is a CLI MCP client that connects to that server; it does not start the server process.

For the MCP signed-transaction path, failure handling is constrained by the signature boundary. The server produces the failure report; the client-hosted agent decides whether to wait, retry the same signed payload while still valid, abandon, or escalate for a newly signed transaction. The policy engine validates the decision before the client calls `submit_signed_bundle`.

## Lifecycle Log

Each JSONL entry includes:

- `submission_id`
- `attempt`
- `bundle_id`
- `signature`
- `tip_lamports`
- submitted, processed, confirmed, finalized timestamps
- submitted, processed, confirmed, finalized slots where available
- latency deltas in milliseconds
- failure classification and raw detail
- ordered stage events

Agent decisions are additionally written to `agent_decisions.log.jsonl` next to the lifecycle log. Each entry contains input state, selected action, policy limits, and final outcome.

## Required README Questions

### 1. What does the delta between `processed_at` and `confirmed_at` tell you about network health at the time of submission?

The delta shows how long it took for a transaction that entered a leader-produced block at processed commitment to receive enough voting lockout to become confirmed. A small delta usually means healthy propagation and voting. A widening delta points to congestion, slow shred propagation, voting lag, fork pressure, or overloaded RPC/stream infrastructure.

### 2. Why should you never use finalized commitment when fetching a blockhash for a time-sensitive transaction?

Finalized blockhashes are too old for latency-sensitive flow. A transaction blockhash has a limited lifetime, and finalized commitment waits for much deeper consensus than processed or confirmed. Fetching at finalized burns useful blockhash lifetime before the transaction is even built, increasing expiry risk.

### 3. What happens to your bundle if the Jito leader skips their slot?

If the connected Jito leader skips the slot, the bundle does not land in that leader's block. Depending on timing and validity, Jito may emit dropped/not-finalized behavior, or the transaction may simply age until its blockhash expires. The sender must detect this and decide whether to refresh the blockhash, adjust tip, and resubmit for a later leader window.

## Notes

This repo intentionally does not import the arbitrage-specific route builder from `arbitrage-rs`. It reuses the useful infrastructure shape: Jito protobufs/auth/tip stream, leader lookup, and Yellowstone streaming. The transaction itself is generic so the bounty stack can be demonstrated without a profitable arbitrage path.

## MCP Tools

The MCP server exposes:

- `get_network_state`: current slot, latest streamed blockhash slot, nearest Jito leader, recent tips, and policy limits.
- `get_recent_tip_data`: recent Jito tip percentiles.
- `classify_failure`: maps raw errors into the failure taxonomy.
- `submit_signed_bundle`: controlled write tool. The server verifies an encoded pre-signed transaction, submits it unchanged as a Jito bundle, tracks lifecycle, classifies failures, and logs the outcome.
- `record_agent_decision`: appends the agent's input state, decision, validation policy, and outcome.

The write surface is intentionally narrow. In the signed transaction flow, the agent can select timing, retry, abandon, or escalate behavior, but it cannot refresh blockhashes, adjust embedded tips, alter arbitrary instructions, or access signing material.

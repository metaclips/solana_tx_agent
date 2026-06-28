# tx_agent - Smart Transaction Stack for Solana

Hackathon: [Superteam Advanced Infrastructure Challenge: Build a Smart Transaction Stack](https://superteam.fun/earn/listing/advanced-infrastructure-challenge-build-a-smart-transaction-stack)

Repository: [https://github.com/metaclips/solana_tx_agent](https://github.com/metaclips/solana_tx_agent)

Submission document: [https://github.com/metaclips/solana_tx_agent/blob/main/docs/hackathon_submission.md](https://github.com/metaclips/solana_tx_agent/blob/main/docs/hackathon_submission.md)

Generated report: [reports/hackathon_report.json](https://github.com/metaclips/solana_tx_agent/blob/main/reports/hackathon_report.json)

## Short Summary

`tx_agent` is a live Solana transaction control plane for Jito bundle submission, lifecycle tracking, and failure reporting.

The stack receives already signed Solana transactions, observes live network state, submits the transaction as a Jito bundle, tracks what happens after submission, classifies failures, and writes the evidence into a structured report.

For the hackathon run, `tx_agent` generated 10 live submission records:

- 7 transactions completed successfully.
- 2 transactions were intentional unfunded failure cases and were correctly classified as `InsufficientFunds`.
- 1 expected-success transaction produced a `SimulationFailure / AlreadyProcessed` classification after the bundle had already landed and after `Processed` / `Confirmed` evidence had been observed. The Jito bundle events show regional forwarding and simulation across multiple locations, with later regional simulations reporting `AlreadyProcessed`. We treat this as a Jito regional reprocessing/idempotency signal, not as an app-level resend https://explorer.jito.wtf/events/8c6f0fab8f9ce1b0a5a4aa5bc15e42856dc555f4008a3955911982cb9202333c.
- All 10 records had slot evidence.
- All 10 records had commitment progression.
- 9 of 10 expected outcomes were met.

The main point is simple: this project is not just a transaction sender. It is a transaction stack that can explain what happened after a signed transaction entered the network.

## The Problem

On Solana, getting a transaction signed is only one part of the job.

A transaction can be validly signed and still fail for reasons that matter to infrastructure teams: stale blockhash, leader timing, insufficient funds, duplicate processing, low embedded tip, Jito bundle behavior, RPC visibility, skipped slots, rate limits, or simulation failure.

A basic sender can often tell you that it attempted to send a transaction. That is not enough for latency-sensitive infrastructure. A useful transaction stack needs to answer practical questions:

- Was the transaction submitted?
- Was it accepted by Jito?
- Which slot was involved?
- Which leader slot was targeted?
- Did it reach processed, confirmed, or finalized commitment?
- Did it fail?
- Why did it fail?
- Is it safe to retry the same signed transaction?
- Does the system need a new signed payload?

`tx_agent` was built around those questions.

## The Solution

`tx_agent` is a controlled transaction execution stack for Solana.

It combines:

- Jito Block Engine integration for live bundle submission.
- Yellowstone/Geyser streams for live slot, blockhash, and signature-status evidence.
- Solana RPC fallback checks for commitment visibility.
- An MCP server that owns all infrastructure access and controlled write tools.
- An MCP client decision host that makes bounded operational decisions.
- A local policy layer that rejects unsafe decisions before any write tool executes.
- JSONL lifecycle records for auditability.
- A generated hackathon report with raw evidence and summarized validation.

The most important design choice is the signing boundary. `tx_agent` does not own private keys. It does not sign transactions. It does not mutate instructions. It does not refresh blockhashes after signing. It does not increase embedded tips inside a signed transaction.

The agent can decide whether to submit now, wait for a leader, retry, abandon, or escalate. If a new blockhash or a higher embedded tip is required, the stack has to escalate to an upstream signer for a new payload. That makes the system safer and more realistic than giving an agent unrestricted wallet control.

<p align="center">
  <img src="https://raw.githubusercontent.com/metaclips/solana_tx_agent/main/docs/images/system-overview.svg" alt="tx_agent system overview" width="100%">
</p>

## Technical Architecture

The project is split into three main layers.

### Core Infrastructure

The core stack lives under `src/core`.

It owns Solana RPC access, Yellowstone/Geyser streaming, Jito Block Engine integration, bundle submission, lifecycle tracking, failure classification, and lifecycle logging. This layer is deliberately the only layer that talks directly to Solana and Jito infrastructure.

Key modules:

- `src/core/config.rs`: runtime configuration.
- `src/core/stack.rs`: transaction verification, submission, and lifecycle orchestration.
- `src/core/lifecycle.rs`: ordered lifecycle records.
- `src/core/networking/geyser.rs`: Yellowstone/Geyser slots, blockhashes, and signature status.
- `src/core/jito/client.rs`: Jito authentication, leader data, tip accounts, bundle submission, and bundle results.
- `src/core/mcp/server.rs`: MCP server and controlled tool surface.

### MCP Control Plane

The MCP server is launched by `agent_mcp` and runs as a streamable HTTP server at `/mcp`.

### Why We Chose MCP

We chose MCP because this project needs a strict line between thinking and doing. The agent can inspect live state and decide what should happen next, but the actual network operations stay behind a small server-owned tool surface.

That design gives us three practical benefits. First, the agent cannot silently expand its own authority; it can only call the tools the MCP server exposes. Second, the infrastructure code remains centralized in one place, which makes Solana RPC, Yellowstone/Geyser, Jito auth, bundle submission, and lifecycle logging easier to audit. Third, every decision and tool call can be recorded with the same request ID, so the final report can connect the agent’s reasoning to the transaction outcome.

For a transaction stack, that boundary matters more than convenience. We did not want an agent with direct wallet access or broad network permissions. MCP lets the agent be useful without letting it become the signer or an uncontrolled transaction sender.

<p align="center">
  <img src="https://raw.githubusercontent.com/metaclips/solana_tx_agent/main/docs/images/mcp-agent-flow.svg" alt="MCP agent flow" width="100%">
</p>

It exposes a narrow set of tools:

- `get_network_state`
- `get_recent_tip_data`
- `classify_failure`
- `submit_signed_bundle`
- `record_agent_decision`

The write path is intentionally narrow. `submit_signed_bundle` accepts an already signed encoded transaction, verifies it, submits it unchanged as a Jito bundle, tracks lifecycle progression, and returns a structured outcome.

### Decision Host

The decision host lives under `src/ai` and is launched by `agent_host`.

It receives an already signed transaction, connects to the MCP server, asks for live state, builds an operational context, selects an action, validates that action through local policy, and then calls the MCP server if the action is allowed.

The allowed actions are:

- `submit_now`
- `wait_for_leader`
- `retry`
- `abandon`
- `escalate`

The decision layer can be useful without being dangerous. It can reason about leader timing, tips, retryability, and failure evidence, but it cannot take control of signing authority.

### Report Generator

The `hackathon_report` binary runs the live demonstration path. It starts the MCP server, generates signed transaction cases, runs the agent host for each case, gathers lifecycle evidence, validates expected outcomes, and writes `reports/hackathon_report.json`.

## Technologies Used

- Rust
- Tokio
- Axum
- RMCP Rust SDK
- Solana SDK
- Solana RPC
- Yellowstone/Geyser gRPC
- Jito Block Engine
- Tonic
- Prost
- Reqwest
- Serde
- JSONL lifecycle logs
- Base64 and Base58 transaction encoding
- SVG diagrams for repository and submission visuals

## How It Works

The end-to-end flow looks like this:

1. A transaction is built and signed outside `tx_agent`.
2. The signed transaction is passed to `agent_host`.
3. `agent_host` connects to the MCP server.
4. `agent_host` calls `get_network_state`.
5. `agent_mcp` returns current slot, nearest Jito leader, tip data, and policy limits.
6. The decision layer chooses an action.
7. The policy layer validates the action.
8. If the action is allowed, `agent_host` calls `submit_signed_bundle`.
9. `agent_mcp` verifies the signed transaction.
10. `TxStack` submits the transaction as a Jito bundle.
11. Jito bundle results, Yellowstone/Geyser signature status, and RPC fallback checks update the lifecycle record.
12. The lifecycle and decision records are written.
13. `hackathon_report` collects the records and writes the final JSON report.

That gives a complete trail from signed payload to observable transaction outcome.

<p align="center">
  <img src="https://raw.githubusercontent.com/metaclips/solana_tx_agent/main/docs/images/transaction-flow.svg" alt="Transaction flow" width="100%">
</p>

## Generated Report
The generated report is committed at [reports/hackathon_report.json](https://github.com/metaclips/solana_tx_agent/blob/main/reports/hackathon_report.json).

| Field | Value |
| --- | --- |
| Run ID | `hackathon-1782676444321` |
| Generated at | `2026-06-28T19:56:17.201473Z` |
| Total submissions | `10` |
| Expected successes | `8` |
| Expected failures | `2` |
| Observed successes | `7` |
| Observed failures reported by classifier | `3` |
| Records with slots | `10` |
| Records with commitment progression | `10` |
| Expected outcomes met | `9` |

This report is the main evidence for the submission. It shows real transaction attempts, not just a static architecture diagram.

## Transaction Results

| Request ID | Expected | Observed result | Classification | Submitted slot | Leader slot |
| --- | --- | --- | --- | --- | --- |
| `hackathon-1782676444321-success-01` | Success | Success | None | `429517795` | `429517795` |
| `hackathon-1782676444321-success-02` | Success | Success | None | `429517831` | `429517831` |
| `hackathon-1782676444321-success-03` | Success | Success | None | `429517875` | `429517876` |
| `hackathon-1782676444321-success-04` | Success | Success | None | `429517912` | `429517912` |
| `hackathon-1782676444321-success-05` | Success | Processed/Confirmed, then duplicate signal | `SimulationFailure / AlreadyProcessed` | `429517948` | `429517948` |
| `hackathon-1782676444321-success-06` | Success | Success | None | `429517972` | `429517972` |
| `hackathon-1782676444321-success-07` | Success | Success | None | `429518013` | `429518016` |
| `hackathon-1782676444321-success-08` | Success | Success | None | `429518050` | `429518052` |
| `hackathon-1782676444321-failure-unfunded-01` | Failure | Failure | `InsufficientFunds` | `429518093` | `429518096` |
| `hackathon-1782676444321-failure-unfunded-02` | Failure | Failure | `InsufficientFunds` | `429518101` | `429518101` |

## Passed Transactions

Seven transactions passed successfully.

These were expected-success transaction cases. Their lifecycle records show normal progression through stages such as `Submitted`, `Accepted`, `Processed`, `Confirmed`, and `Finalized`.

For each successful transaction, the report preserved the request ID, transaction signature, submitted slot, leader slot, observed tip amount, lifecycle events, commitment progression, and matched lifecycle validation.

This matters because the system is not only claiming that a transaction was sent. It is recording the path that proves the transaction moved through the network.

<p align="center">
  <img src="https://raw.githubusercontent.com/metaclips/solana_tx_agent/main/docs/images/jito-bundle-lifecycle.svg" alt="Jito bundle submission lifecycle" width="100%">
</p>

## Failed Transactions

The report contains two intentional transaction failures and one duplicate/idempotency signal.

Two were intentional expected-failure cases. They used unfunded transactions and were classified as `InsufficientFunds`. The raw error showed that the transaction attempted to debit an account without a prior credit. These failures were part of the test plan, and both were correctly detected and classified.

The third classified case was `hackathon-1782676444321-success-05`. It was expected to succeed, and the lifecycle record shows `Processed` and `Confirmed` evidence before the `AlreadyProcessed` classification appeared. The Jito bundle event view also shows the bundle landed in slot `429517952` with a `0.000001 SOL` landed tip, while regional auction simulations and forwards were happening across locations such as Frankfurt, London, Amsterdam, New York, Dublin, Singapore, SLC, and Tokyo. Later regional auction simulations in Frankfurt and Dublin reported `AlreadyProcessed`.

The hackathon run did not retry this transaction at the application level: `MAX_AGENT_RETRIES` was set to `0`, the decision log has a single `submit_now` action at attempt `0`, and the lifecycle log has one bundle ID for that request. The right interpretation is that Jito's regional distribution/result path attempted to process or simulate the already-landed signature from another location, then correctly saw that the transaction had already been processed.

That is useful transaction infrastructure behavior. In production, the job is not to flatten every edge case into success or failure. The job is to preserve enough evidence to distinguish a real transaction failure, like `InsufficientFunds`, from a Jito regional duplicate/reprocessing signal, like `AlreadyProcessed`.

<p align="center">
  <img src="https://raw.githubusercontent.com/metaclips/solana_tx_agent/main/docs/images/failure-handling-strategy.svg" alt="Failure handling strategy" width="100%">
</p>

## How To Run It

Build the binaries:

```sh
cargo build --bins
```

Generate the report:

```sh
target/debug/hackathon_report \
  --rpc-url "$SOLANA_RPC_URL" \
  --yellowstone-endpoint "$YELLOWSTONE_ENDPOINT" \
  --jito-auth-keypair "$JITO_AUTH_KEYPAIR" \
  --payer-keypair "$TX_AGENT_REAL_PAYER_KEYPAIR" \
  --out reports/hackathon_report.json
```

The report generator expects these environment values or equivalent CLI flags:

| Variable | Purpose |
| --- | --- |
| `SOLANA_RPC_URL` | Solana RPC endpoint for blockhash and commitment fallback checks. |
| `YELLOWSTONE_ENDPOINT` | Yellowstone/Geyser endpoint for live slots, blockhashes, and signature status. |
| `YELLOWSTONE_TOKEN` | Provider token when the Yellowstone endpoint requires one. |
| `JITO_AUTH_KEYPAIR` | Jito auth keypair used by the MCP server. |
| `TX_AGENT_REAL_PAYER_KEYPAIR` | Funded payer keypair used to create the live success cases. |

Manual MCP flow:

```sh
cargo run --bin agent_mcp -- --bind 127.0.0.1:8080
```

Then submit a pre-signed transaction through the client host:

```sh
cargo run --bin agent_host -- \
  --mcp-url http://127.0.0.1:8080/mcp \
  --request-id demo-001 \
  --encoding base64 \
  --transaction-file signed-tx.base64 \
  --observed-tip-lamports 1000
```

The live report path performs real Jito bundle submissions. The payer keypair should be funded only when the operator intentionally wants to spend transaction fees and tips.

## What Makes It Useful

`tx_agent` treats transaction submission as a lifecycle, not a single send call.

That changes the shape of the infrastructure. Instead of ending at "transaction sent", the stack records bundle IDs, signatures, submitted slots, leader slots, tip amounts, lifecycle timestamps, commitment progression, failure classification, and raw failure details.

It is also safer than an agent with direct wallet power. The decision host can make useful operational choices, but it cannot rewrite a transaction or take over signing. When a signed transaction is no longer suitable, the correct action is escalation to the signer, not silent mutation inside the agent.

## README Questions

### What does the delta between `processed_at` and `confirmed_at` tell you about network health at the time of submission?

The delta measures how long it took after the transaction first appeared at processed commitment for enough stake-weighted voting to confirm it. A small delta usually means the leader produced the block, shreds propagated quickly, validators voted promptly, and the RPC/Geyser view was healthy. A widening delta points to network stress: slow shred propagation, voting lag, fork pressure, overloaded RPC infrastructure, or congestion that delays confirmation even after a transaction is first observed.

In this stack, that delta is useful because `processed_at` is the earliest practical landing signal, while `confirmed_at` is the stronger signal that the cluster is converging on that block. The report preserves both timestamps and slot numbers so the delta can be compared across submissions in the same run.

### Why should you never use finalized commitment when fetching a blockhash for a time-sensitive transaction?

Finalized commitment is too old for latency-sensitive submission. A Solana transaction blockhash has a limited lifetime. Fetching a finalized blockhash waits for deep consensus before the transaction is even built, which wastes part of the validity window and increases the chance that the transaction expires before it reaches a leader.

For Jito bundle flow, that is especially harmful because the sender may wait for a connected leader window. Using a fresh processed or confirmed blockhash preserves more lifetime for signing, routing, bundle forwarding, and leader execution.

### What happens to your bundle if the Jito leader skips their slot?

If the targeted Jito leader skips its slot, the bundle cannot land in that leader's block because no block was produced for that opportunity. Depending on timing and blockhash lifetime, the bundle may be dropped, remain unlanded until it expires, or need to be resent for a later leader window.

This stack treats that as an operational failure path. It records the slot and bundle lifecycle evidence, classifies any returned Jito/Solana failure detail, and lets the decision layer choose whether to wait, retry the same still-valid signed payload, abandon, or escalate for a new signed transaction with a fresh blockhash and possibly a different tip.

## Current Limitations

`tx_agent` currently expects already signed transactions. It does not build arbitrary user transactions, hold wallet authority, refresh blockhashes after signing, or increase embedded tips inside an existing signed payload.

Some recovery paths therefore require an upstream signer. That is intentional for this version. It keeps the authority boundary clear, but it also means the system needs a signer integration for fully automated recovery from expired blockhashes or tip changes.

Live results also depend on Solana network conditions, Jito leader timing, RPC behavior, Yellowstone/Geyser availability, and block engine responses. The report includes one unexpected live failure, which is a realistic example of why the lifecycle evidence matters.

## Future Improvements

- Add a signer callback interface for controlled escalation.
- Add a small dashboard for report inspection.
- Expand failure classification with more Solana and Jito edge cases.
- Add a Jito bundle outcome reconciler for landed-then-`AlreadyProcessed` cases. The reconciler should merge Jito bundle events, regional auction simulation results, RPC commitment evidence, and Yellowstone/Geyser status before assigning the final outcome. If a bundle has landed or the signature is already `Processed` / `Confirmed`, later regional `AlreadyProcessed` simulation errors should be downgraded from failure to a benign duplicate/reprocessing signal.
- Add richer attribution for which lifecycle evidence came from Geyser versus RPC fallback.
- Add more integration tests for retry, timeout, and leader-skip paths.
- Add a dry-run mode for demos that should not spend fees or tips.
- Add configurable tip and leader strategies for different transaction classes.

## Conclusion

`tx_agent` is a practical smart transaction stack for Solana.

It submitted real transactions through Jito, tracked lifecycle progression, recorded slot and commitment evidence, classified failures, and generated a report that judges can inspect.

The hackathon run produced 10 live submission records: 7 clean successful transactions, 2 intentional `InsufficientFunds` failures, and 1 `AlreadyProcessed` duplicate signal after processed/confirmed evidence. The edge cases are not brushed aside. They are part of the point. A useful transaction stack should make transaction outcomes understandable.

The project gives an agent enough control to make operational decisions, but not enough authority to become the signer. That boundary is what makes the design usable for serious transaction infrastructure.

# tx_agent Architecture

This document is the source draft for the public architecture document required by the bounty. Publish this content to Notion, Google Docs, or Figma and include the public URL in the final submission.

## System Overview

`tx_agent` is a transaction infrastructure stack for Solana. It observes network state through Yellowstone/Geyser, times submissions around connected Jito leaders, sends real Jito bundles, tracks lifecycle progression, classifies failures, and delegates retry decisions to an AI agent.

## Components

### CLI

Entrypoint: `src/main.rs`

Modes:

- `submit --count N`: submit N real bundles.
- `fault expired-blockhash`: force one expired-blockhash failure and let the agent retry.
- `print-log`: summarize lifecycle evidence.

### Config

Module: `src/config.rs`

Reads RPC, Yellowstone, Jito, payer, logging, tip, and AI settings from environment variables.

### Yellowstone/Geyser Stream

Module: `src/networking/geyser.rs`

Responsibilities:

- Stream slots and block metadata.
- Maintain current slot and latest streamed blockhash.
- Subscribe to exact transaction signature status using `transactions_status`.
- Reconnect with exponential backoff.

### Jito Client

Module: `src/jito/client.rs`

Responsibilities:

- Authenticate with the Jito block engine.
- Subscribe to bundle result stream.
- Fetch connected Jito leaders.
- Fetch Jito tip accounts.
- Ingest live tip-floor data.
- Submit bundles and handle rate-limit retries.
- Broadcast bundle lifecycle events to the stack.

### Transaction Factory

Module: `src/tx_factory.rs`

Builds a signed Solana v0 transaction containing:

- A tiny self-transfer, making the transaction independently inspectable.
- A Jito tip transfer to a live tip account.

### Lifecycle Tracker

Module: `src/lifecycle.rs`

Records:

- Bundle ID and signature.
- Submitted, accepted, processed, confirmed, finalized events.
- Timestamps and slots.
- Latency deltas.
- Failure classification.
- Agent decision.

The output is JSONL for simple auditability.

### AI Agent

Module: `src/agent/mod.rs`

Decision owned by the agent: retry behavior after failure.

Input evidence:

- Failure kind and raw details.
- Current slot and leader slot.
- Previous tip.
- Blockhash age.
- Live Jito tip percentiles.

Output:

- `retry`, `hold`, or `abort`.
- Whether to refresh blockhash.
- Next tip amount.
- Reasoning string.

If `OPENAI_API_KEY` is set, the agent calls an OpenAI-compatible chat-completions endpoint and validates the structured JSON response. Otherwise, it uses deterministic fallback reasoning and marks the source as `fallback_reasoner`.

## Data Flow

1. CLI loads config and starts `TxStack`.
2. `TxStack` starts Yellowstone slot/blockhash streaming.
3. Jito client authenticates, subscribes to bundle results, fetches leaders, fetches tip accounts, and starts tip stream.
4. Stack waits for a Jito leader within `LEADER_LOOKAHEAD_SLOTS`.
5. Stack fetches a processed blockhash from RPC.
6. Agent computes initial tip from live Jito tip data and slot pressure.
7. Transaction factory builds and signs the transaction.
8. Jito client submits the transaction as a bundle.
9. Stack opens a Yellowstone signature-status subscription.
10. Jito bundle events and Yellowstone signature events update the lifecycle record.
11. RPC commitment polling fills in confirmed/finalized fallback evidence.
12. On failure, the stack classifies the reason and passes evidence to the AI agent.
13. Agent decides retry/hold/abort.
14. If retry, stack refreshes blockhash, recalculates tip, and resubmits.

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

Expired blockhash is handled by refreshing the processed blockhash, recalculating tip, and resubmitting only if the agent chooses retry.

Fee/tip failures increase tip toward higher live Jito percentiles.

Compute and simulation failures default to abort because blockhash/tip changes do not fix transaction construction errors.

## Infrastructure Decisions

- Yellowstone is primary for slot and signature lifecycle evidence.
- RPC is used for blockhash fetch and commitment fallback.
- Processed commitment is used for blockhash fetch to preserve lifetime.
- Jito connected leader data is used before submission to avoid blind sending.
- Lifecycle evidence is append-only JSONL for auditability.
- Agent decisions are stored inside the lifecycle record for judging.

## Suggested Diagram

```text
             +-------------------+
             |       CLI         |
             +---------+---------+
                       |
                       v
             +-------------------+
             |      TxStack      |
             +---+-----------+---+
                 |           |
                 v           v
       +--------------+   +----------------+
       | Yellowstone  |   |  Jito Client   |
       | Slot/Status  |   | Leaders/Bundle |
       +------+-------+   +---+------------+
              |               |
              v               v
        +-----------+   +-------------+
        | Lifecycle |<--| Bundle Feed |
        | Logger    |   +-------------+
        +-----+-----+
              |
              v
        +-----------+
        | AI Agent  |
        | Retry     |
        +-----------+
```

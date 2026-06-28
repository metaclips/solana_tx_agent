# tx_agent

Smart Solana transaction infrastructure prototype for the hackathon bounty. It combines Yellowstone/Geyser slot and signature-status streams, Jito bundle submission, live Jito tip data, lifecycle logging, and an AI-owned retry decision.

## What It Does

- Streams live slots and blockhashes from Yellowstone/Geyser.
- Fetches Jito connected leader windows and waits for a near leader before submission.
- Builds a real signed Solana v0 transaction with a Jito tip transfer.
- Submits the transaction as a Jito bundle.
- Tracks bundle events from Jito and signature status from Yellowstone.
- Uses RPC commitment checks as fallback/corroboration, not as the only lifecycle source.
- Logs submitted, accepted, processed, confirmed, finalized, failure, latency, slot, and tip data to JSONL.
- Supports fault injection for an expired blockhash path.
- Lets the AI agent decide retry/hold/abort after failure.

## Setup

Required environment:

```sh
export YELLOWSTONE_ENDPOINT="https://your-yellowstone-endpoint"
export YELLOWSTONE_TOKEN="optional-token"
export SOLANA_RPC_URL="https://your-rpc"
export PAYER_KEYPAIR="$HOME/.config/solana/id.json"
export JITO_AUTH_KEYPAIR="$HOME/.config/solana/jito-auth.json"
```

Optional environment:

```sh
export JITO_BLOCK_ENGINE_URL="https://frankfurt.mainnet.block-engine.jito.wtf"
export LIFECYCLE_LOG="lifecycle.log.jsonl"
export SUBMIT_COUNT=10
export LEADER_LOOKAHEAD_SLOTS=3
export TIP_FLOOR_LAMPORTS=1000
export SELF_TRANSFER_LAMPORTS=1
export CONFIRMATION_TIMEOUT_SECS=90
export OPENAI_API_KEY="optional"
export OPENAI_MODEL="gpt-4.1-mini"
```

The payer must be funded for normal Solana transaction fees plus Jito tips. If `JITO_AUTH_KEYPAIR` is not set, the payer keypair is used for Jito auth.

## Commands

```sh
cargo run -- submit --count 10
cargo run -- fault expired-blockhash
cargo run -- print-log
```

`fault expired-blockhash` deliberately builds the first attempt with `Hash::default()`. The expected flow is failure classification, agent reasoning, blockhash refresh, tip recalculation, and autonomous resubmission.

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
- agent retry decision
- ordered stage events

Print a compact view:

```sh
cargo run -- print-log
```

## Required README Questions

### 1. What does the delta between `processed_at` and `confirmed_at` tell you about network health at the time of submission?

The delta shows how long it took for a transaction that entered a leader-produced block at processed commitment to receive enough voting lockout to become confirmed. A small delta usually means healthy propagation and voting. A widening delta points to congestion, slow shred propagation, voting lag, fork pressure, or overloaded RPC/stream infrastructure.

### 2. Why should you never use finalized commitment when fetching a blockhash for a time-sensitive transaction?

Finalized blockhashes are too old for latency-sensitive flow. A transaction blockhash has a limited lifetime, and finalized commitment waits for much deeper consensus than processed or confirmed. Fetching at finalized burns useful blockhash lifetime before the transaction is even built, increasing expiry risk.

### 3. What happens to your bundle if the Jito leader skips their slot?

If the connected Jito leader skips the slot, the bundle does not land in that leader's block. Depending on timing and validity, Jito may emit dropped/not-finalized behavior, or the transaction may simply age until its blockhash expires. The sender must detect this and decide whether to refresh the blockhash, adjust tip, and resubmit for a later leader window.

## Notes

This repo intentionally does not import the arbitrage-specific route builder from `arbitrage-rs`. It reuses the useful infrastructure shape: Jito protobufs/auth/tip stream, leader lookup, and Yellowstone streaming. The transaction itself is generic so the bounty stack can be demonstrated without a profitable arbitrage path.

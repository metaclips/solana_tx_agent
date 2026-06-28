# src/ai/agent

AI-assisted operational decision layer.

The agent owns one bounded decision: submit now, wait for leader, retry, abandon, or escalate. It does not sign transactions, mutate signed payloads, access private keys, or call Solana/Jito directly.

When the transaction is already signed, blockhash refreshes and tip changes require escalation for a newly signed transaction.

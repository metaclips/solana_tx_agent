# src/core/mcp

MCP boundary implementation using `modelcontextprotocol/rust-sdk` via the `rmcp` crate.

- `host.rs` is the MCP client and agent host. It accepts an encoded signed transaction, connects to a running MCP HTTP server, asks the agent for a bounded decision, validates policy, and calls server tools.
- `server.rs` is the long-running MCP streamable HTTP server and Solana control plane. It exposes live network state, failure classification, audit logging, and signed Jito bundle submission tools.
- `mod.rs` exports the host and server modules.

The flow is `agent_host` -> HTTP MCP `/mcp` -> `submit_signed_bundle` -> Jito. The server verifies and submits the signed payload unchanged.

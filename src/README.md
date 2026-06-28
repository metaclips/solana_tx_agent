# src

Rust source for `tx_agent`.

- `bin/` contains thin binary launchers.
- `mcp/` contains the MCP client host and MCP server.
- `agent/` contains operational decision logic.
- `jito/` contains Jito block-engine integration.
- `networking/` contains Yellowstone/Geyser streaming.
- `stack.rs` coordinates Solana, Jito, lifecycle, and agent-facing operations.

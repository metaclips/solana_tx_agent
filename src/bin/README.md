# src/bin

Thin binary entrypoints.

- `agent_host.rs` launches the MCP client/agent host in `src/ai/mcp_host.rs`.
- `agent_mcp.rs` launches the MCP server/control plane in `src/core/mcp/server.rs`.

Keep implementation logic out of this folder so the binaries stay easy to scan.

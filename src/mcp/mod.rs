//! MCP client and server boundary.
//!
//! The binaries in `src/bin` are thin launchers. The implementation lives here
//! so the MCP control plane is easy to review separately from the core Solana
//! transaction stack.

pub mod host;
pub mod server;

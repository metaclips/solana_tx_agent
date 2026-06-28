//! Core Solana/Jito transaction infrastructure.
//!
//! This parent module contains code that owns network connections, lifecycle
//! logging, bundle submission, and the MCP control plane.

pub mod config;
pub mod jito;
pub mod lifecycle;
pub mod mcp;
pub mod networking;
pub mod stack;

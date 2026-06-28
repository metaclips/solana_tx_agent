//! AI-facing operational decision and host orchestration code.
//!
//! This parent module is intentionally separate from the core stack. It may
//! decide when to submit, wait, retry, abandon, or escalate, but it does not own
//! Solana/Jito connections or signing material.

pub mod agent;
pub mod mcp_host;
pub mod policy;

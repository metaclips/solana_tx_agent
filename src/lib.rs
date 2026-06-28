//! Core tx_agent library modules.
//!
//! This crate implements the transaction lifecycle stack for Solana using
//! Yellowstone/Geyser slot streams, Jito bundle submission, and an AI-assisted
//! decision agent. The implementation is adapted from existing `arbitrage-rs`
//! infrastructure code.

pub mod agent;
pub mod config;
pub mod jito;
pub mod lifecycle;
pub mod mcp;
pub mod networking;
pub mod policy;
pub mod stack;

//! Yellowstone/Geyser streaming support for tx_agent.
//!
//! This module provides live Solana slot and blockhash event subscriptions via
//! a compatible Yellowstone gRPC endpoint. It is intended to power low-latency
//! slot observation for Jito bundle submission.

pub mod geyser;

pub use geyser::{Geyser, GeyserStream};

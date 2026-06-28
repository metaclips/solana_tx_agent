# src/networking

Yellowstone/Geyser streaming integration.

- `geyser.rs` streams slots, blockhashes, and transaction signature status.
- `mod.rs` exports the networking module.

The lifecycle tracker uses this data as primary evidence for slot and transaction progression.

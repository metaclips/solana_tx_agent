# src/jito

Jito block-engine integration.

- `client.rs` authenticates, tracks connected leaders, subscribes to bundle results, fetches tip accounts, and submits bundles.
- `interceptor.rs` handles authenticated gRPC requests.
- `tip.rs` ingests recent Jito tip data.
- `protos.rs` exposes generated protobuf modules.

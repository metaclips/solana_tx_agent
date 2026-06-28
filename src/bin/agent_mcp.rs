#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tx_agent::core::mcp::server::run().await
}

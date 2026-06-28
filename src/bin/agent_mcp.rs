#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tx_agent::mcp::server::run().await
}

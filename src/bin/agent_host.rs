#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tx_agent::ai::mcp_host::run().await
}

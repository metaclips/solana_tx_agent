use tracing_subscriber::{EnvFilter, fmt};
use tx_agent::{
    config::{Config, Mode},
    lifecycle::LifecycleLogger,
    stack::TxStack,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    fmt().with_env_filter(EnvFilter::from_default_env()).init();

    let config = Config::from_env_and_args()?;
    match config.mode.clone() {
        Mode::PrintLog => LifecycleLogger::print(&config.lifecycle_log_path),
        Mode::Run => {
            let stack = TxStack::connect(config.clone()).await?;
            stack.submit_many(config.submit_count, false).await
        }
        Mode::Submit { count } => {
            let stack = TxStack::connect(config).await?;
            stack.submit_many(count, false).await
        }
        Mode::FaultExpiredBlockhash => {
            let stack = TxStack::connect(config).await?;
            stack.submit_many(1, true).await
        }
    }
}

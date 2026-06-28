use std::{
    env,
    net::{SocketAddr, TcpStream},
    path::PathBuf,
    process::{Command, Output},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};

use crate::agent::AgentGuard;

pub(crate) fn sibling_binary(name: &str) -> anyhow::Result<PathBuf> {
    let current = env::current_exe()?;
    let exe_name = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let path = current
        .parent()
        .context("current executable has no parent directory")?
        .join(exe_name);
    if !path.exists() {
        bail!(
            "required sibling binary {} does not exist; run `cargo build --bins` first",
            path.display()
        );
    }
    Ok(path)
}

pub(crate) fn wait_for_agent(
    bind_addr: &str,
    timeout: Duration,
    agent: &mut AgentGuard,
) -> anyhow::Result<()> {
    let addr: SocketAddr = bind_addr
        .parse()
        .with_context(|| format!("invalid bind address {bind_addr}"))?;
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok() {
            return Ok(());
        }
        if let Some(status) = agent.try_wait()? {
            bail!("agent_mcp exited before opening {bind_addr}: {status}");
        }
        thread::sleep(Duration::from_millis(500));
    }
    bail!("agent_mcp did not become ready at {bind_addr} within {timeout:?}");
}

pub(crate) fn run_with_timeout(mut command: Command, timeout: Duration) -> anyhow::Result<Output> {
    let mut child = command.spawn()?;
    let start = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            return Ok(child.wait_with_output()?);
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            bail!("command timed out after {timeout:?}");
        }
        thread::sleep(Duration::from_millis(500));
    }
}

pub(crate) fn next_arg(
    iter: &mut impl Iterator<Item = String>,
    name: &str,
) -> anyhow::Result<String> {
    iter.next()
        .with_context(|| format!("{name} requires a value"))
}

pub(crate) fn env_u64(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
}

use agent_box_common::config::load_config;
use clap::Parser;
use eyre::Result;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tracing::error;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "agent-portal-host")]
#[command(about = "Host portal service for container capability requests")]
struct Cli {
    /// Override socket path
    #[arg(long)]
    socket: Option<String>,
}

fn init_logging() {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,agent_portal_host=info"));

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_ansi(std::io::stderr().is_terminal())
        .init();
}

fn main() {
    init_logging();

    if let Err(e) = run() {
        error!(error = %e, "portal host failed");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let path = std::env::var("PATH").unwrap_or_default();
    let path = path.split(':').collect::<Vec<_>>();
    tracing::info!(path = ?path, "PATH");

    let cli = Cli::parse();
    let config = load_config()?;
    let portal = config.portal;
    let socket_path = PathBuf::from(cli.socket.unwrap_or_else(|| portal.socket_path.clone()));

    agent_portal::host::run_with_config_and_socket(
        portal,
        socket_path,
        Arc::new(AtomicBool::new(false)),
    )
}

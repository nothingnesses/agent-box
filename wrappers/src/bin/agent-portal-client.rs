use agent_box_common::portal_client::PortalClient;
use clap::{Parser, Subcommand};
use eyre::Result;
use std::io::Write;

#[derive(Parser, Debug)]
#[command(name = "agent-portal-client")]
#[command(about = "Lightweight portal client for wrappers/scripts")]
struct Cli {
    #[arg(long)]
    socket: Option<String>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    ListTypes,
    ReadImage {
        #[arg(long)]
        mime: Option<String>,
        #[arg(long)]
        reason: Option<String>,
    },
}

fn main() {
    if let Err(e) = run() {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let client = if let Some(s) = cli.socket {
        PortalClient::with_socket(s)
    } else {
        PortalClient::from_env_or_config()
    };

    match cli.command {
        Commands::ListTypes => {
            let image = client.clipboard_read_image(Some("wrapper:list-types".to_string()))?;
            println!("{}", image.mime);
        }
        Commands::ReadImage { mime, reason } => {
            let image = client.clipboard_read_image(reason)?;
            if let Some(requested) = mime
                && requested != image.mime
            {
                return Err(eyre::eyre!(
                    "requested mime {} not currently available (got {})",
                    requested,
                    image.mime
                ));
            }
            std::io::stdout().write_all(&image.bytes)?;
        }
    }

    Ok(())
}

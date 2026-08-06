//! ccp CLI entry point.

use ccp::paths::Paths;
use ccp::{presets, web};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "ccp",
    about = "Claude Code Profiles — isolated per-provider configs"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start the local web GUI server (default command).
    Serve {
        /// Port override. Precedence: --port > CCP_PORT > config.toml > built-in default.
        #[arg(long)]
        port: Option<u16>,
    },
    /// Print the built-in provider preset table.
    Presets,
}

const DEFAULT_PORT: u16 = 9847;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let paths = Paths::from_env()?;

    match cli.cmd.unwrap_or(Cmd::Serve { port: None }) {
        Cmd::Presets => {
            for p in presets::load() {
                println!("{:<16} {:<14} {}", p.key, p.category, p.label);
            }
            Ok(())
        }
        Cmd::Serve { port } => {
            let port = resolve_port(&paths, port);
            web::serve(paths, port).await
        }
    }
}

fn resolve_port(paths: &Paths, flag: Option<u16>) -> u16 {
    if let Some(p) = flag {
        return p;
    }
    if let Some(p) = std::env::var("CCP_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
    {
        return p;
    }
    if let Ok(raw) = std::fs::read_to_string(paths.config_file()) {
        #[derive(serde::Deserialize)]
        struct Cfg {
            port: Option<u16>,
        }
        if let Ok(cfg) = toml::from_str::<Cfg>(&raw) {
            if let Some(p) = cfg.port {
                return p;
            }
        }
    }
    DEFAULT_PORT
}

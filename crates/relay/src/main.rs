use clap::{Parser, Subcommand};
use codex_relay::{
    admin, desktop,
    error::{Error, Result},
    installation,
};
use std::path::PathBuf;
mod server_config;

#[derive(Parser)]
#[command(version, about = "Codex Relay · Codex 中转站 — 原生客户端推理中转服务")]
struct Cli {
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    Serve {
        #[arg(long)]
        port: Option<u16>,
        /// Print the resolved port without starting the server (for launchers).
        #[arg(long)]
        print_port: bool,
        #[arg(long, default_value = "web/dist")]
        web_dir: PathBuf,
        /// JSON file enabling separate client HTTP/HTTPS listeners.
        #[arg(long)]
        network_config: Option<PathBuf>,
        /// Require a random per-start management token in addition to configured login.
        #[arg(long)]
        require_admin_token: bool,
    },
    Prepare {
        #[arg(long)]
        desktop_pid: Option<u32>,
    },
    Doctor,
    Inspect,
    InspectProvider,
    Probe {
        #[arg(value_parser=["shared","guard","stream","provider"])]
        kind: String,
    },
    Launch {
        #[arg(long, default_value_t = 4500)]
        backend_port: u16,
        #[arg(long, default_value_t = 4501)]
        desktop_port: u16,
        #[arg(long)]
        verify_lan: bool,
    },
    Status,
}
#[tokio::main]
async fn main() {
    if let Err(error) = run(Cli::parse()).await {
        eprintln!("{}", serde_json::json!({"error":error}));
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    let value = match cli.command {
        Action::Serve {
            port,
            print_port,
            web_dir,
            network_config,
            require_admin_token,
        } => {
            let port =
                server_config::resolve_port(std::path::Path::new("relay.config.json"), port)?;
            if print_port {
                println!("{port}");
                return Ok(());
            }
            if !web_dir.join("index.html").is_file() {
                return Err(Error::new(
                    "WEB_BUILD_REQUIRED",
                    "请先执行 pnpm run build:web。",
                ));
            }
            let network = match network_config {
                Some(path) => codex_relay::network::NetworkConfig::read(&path).await?,
                None => codex_relay::network::NetworkConfig::default(),
            };
            admin::serve_with_network(port, web_dir, network, require_admin_token).await?;
            return Ok(());
        }
        Action::Launch {
            backend_port,
            desktop_port,
            verify_lan,
        } => {
            desktop::launch(backend_port, desktop_port, verify_lan).await?;
            return Ok(());
        }
        Action::Prepare { desktop_pid } => {
            serde_json::to_value(installation::prepare(desktop_pid).await?)?
        }
        Action::Doctor => admin::run_job("doctor").await?,
        Action::InspectProvider => admin::run_job("inspect-provider").await?,
        Action::Probe { kind } => admin::run_job(&kind).await?,
        Action::Status => desktop::status().await,
        Action::Inspect => {
            let install = installation::read().await?;
            let (_, archive) = install.desktop_paths()?;
            let inspection = installation::inspect_archive(archive)?;
            serde_json::json!({"files":inspection.source_files,"sharedWebSocketOverride":inspection.shared_websocket,"nativeClientInfo":inspection.native_client_info})
        }
    };
    println!("{}", serde_json::to_string_pretty(&value)?);
    if value["checks"]
        .as_array()
        .is_some_and(|a| a.iter().any(|c| c["state"] == "blocked"))
    {
        std::process::exit(2);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn management_token_is_opt_in() {
        for (args, required) in [
            (vec!["bridge", "serve"], false),
            (vec!["bridge", "serve", "--require-admin-token"], true),
        ] {
            let cli = Cli::try_parse_from(args).unwrap();
            assert!(
                matches!(cli.command, Action::Serve { require_admin_token, .. } if require_admin_token == required)
            );
        }
    }
}

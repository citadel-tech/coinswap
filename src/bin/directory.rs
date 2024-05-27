use clap::{Parser, Subcommand};
use coinswap::{
    market::directory::{start_directory_server, DirectoryServer},
    utill::{get_dns_dir, setup_logger, ConnectionType},
};
use std::{path::PathBuf, sync::Arc};

/// The DNS Server.
///
/// This app starts the DNS server to serve Maker addresses to the Taker clients.
#[derive(Parser)]
#[clap(version = option_env ! ("CARGO_PKG_VERSION").unwrap_or("unknown"),
author = option_env ! ("CARGO_PKG_AUTHORS").unwrap_or(""))]
struct Cli {
    /// Top level subcommands
    #[clap(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Starts the directory server
    Start {
        /// Optional DNS data directory. Default value : "~/.coinswap/directory"
        #[clap(long, short = 'd')]
        data_directory: Option<PathBuf>,
        /// Optional network type.
        #[clap(long, short = 'n', default_value = "clearnet", possible_values = &["tor", "clearnet"])]
        network: String,
    },
}

fn main() {
    setup_logger();

    let cli = Cli::parse();

    match cli.command {
        Commands::Start {
            data_directory,
            network,
        } => {
            let network_type = match network.as_str() {
                "tor" => ConnectionType::TOR,
                _ => ConnectionType::CLEARNET,
            };

            let data_directory = data_directory.unwrap_or(get_dns_dir());
            let directory_server = DirectoryServer::new(
                Some(&data_directory.join("config.toml")),
                Some(network_type),
            )
            .unwrap();
            let arc_directory_server = Arc::new(directory_server);

            start_directory_server(arc_directory_server);
        }
    }
}

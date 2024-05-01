use clap::{Parser, Subcommand};
use coinswap::{
    market::directory::{start_directory_server, DirectoryServer},
    utill::{setup_logger, ConnectionType},
};
use std::{path::PathBuf, sync::Arc};

#[derive(Parser)]
#[clap(
    name = "directory-server",
    about = "A simple directory server.",
    version
)]
struct Cli {
    #[clap(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Starts the directory server
    Start {
        #[clap(long, value_parser, default_value = "directory.toml")]
        data_directory: PathBuf,

        #[clap(long, default_value = "clearnet")]
        network: String,
    },
}

fn main() {
    setup_logger();

    let cli = Cli::parse();

    match &cli.command {
        Commands::Start {
            data_directory,
            network,
        } => {
            let network_type = match network.as_str() {
                "tor" => ConnectionType::TOR,
                _ => ConnectionType::CLEARNET,
            };

            let directory_server =
                DirectoryServer::new(Some(data_directory), Some(network_type)).unwrap();
            let arc_directory_server = Arc::new(directory_server);

            start_directory_server(arc_directory_server);
        }
    }
}

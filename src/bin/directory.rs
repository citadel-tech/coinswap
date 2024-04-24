use clap::{Parser, Subcommand};
use coinswap::{
    market::directory::{start_directory_server, DirectoryServer},
    utill::{setup_logger, ConnectionType},
};
use std::{path::PathBuf, sync::Arc};

#[derive(Parser, Debug)]
#[clap(
    name = "directory-server",
    about = "A simple directory server.",
    version
)]
struct Cli {
    #[clap(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Start {
        #[clap(long, value_parser)]
        data_directory: Option<PathBuf>,
        #[clap(long, value_parser)]
        network: Option<ConnectionType>,
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
            log::info!("Data directory : {:?}", data_directory);
            let directory_server = match data_directory {
                Some(data_dir) => DirectoryServer::new(Some(data_dir), *network),
                None => DirectoryServer::new(None, *network),
            };

            match directory_server {
                Ok(server) => {
                    let arc_server = Arc::new(server);
                    start_directory_server(arc_server);
                }
                Err(e) => println!("Failed to initialize directory server: {}", e),
            }
        }
    }
}

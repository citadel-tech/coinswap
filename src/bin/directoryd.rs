use clap::Parser;
use coinswap::{
    market::directory::{start_directory_server, DirectoryServer},
    utill::{setup_logger, read_connection_network_string},
};
use std::{path::PathBuf, sync::Arc};

/// The DNS Server.
///
/// This app starts the DNS server to serve Maker addresses to the Taker clients.

#[derive(Parser)]
#[clap(version = option_env ! ("CARGO_PKG_VERSION").unwrap_or("unknown"),
author = option_env ! ("CARGO_PKG_AUTHORS").unwrap_or(""))]

struct Cli {
    /// Optional network type.
    #[clap(long, short = 'n', default_value = "clearnet", possible_values = &["tor", "clearnet"])]
    network: String,
    /// Optional DNS data directory. Default value : "~/.coinswap/directory"
    #[clap(long, short = 'd')]
    data_directory: Option<PathBuf>,
}

fn main() {
    setup_logger();
    log::info!("Starting Directory Server");

    let args = Cli::parse();

    let conn_type = read_connection_network_string(&args.network).unwrap();

    let directory = Arc::new(
        DirectoryServer::new(
            args.data_directory,
            Some(conn_type), 
        ). unwrap(),
    );

    start_directory_server(directory);

}
use clap::Parser;
use coinswap::{
    market::directory::{start_directory_server, DirectoryServer},
    utill::{read_connection_network_string, setup_logger},
};
use std::{path::PathBuf, sync::Arc};

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
    setup_logger(log::LevelFilter::Info);

    let args = Cli::parse();

    let conn_type = read_connection_network_string(&args.network).unwrap();

    let directory = Arc::new(DirectoryServer::new(args.data_directory, Some(conn_type)).unwrap());

    start_directory_server(directory);
}

use clap::Parser;
use coinswap::{
    market::directory::{start_directory_server, DirectoryServer, DirectoryServerError},
    utill::{setup_logger, ConnectionType},
};
use std::{path::PathBuf, str::FromStr, sync::Arc};

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

fn main() -> Result<(), DirectoryServerError> {
    setup_logger(log::LevelFilter::Info);

    let args = Cli::parse();

    let conn_type = ConnectionType::from_str(&args.network)?;

    let directory = Arc::new(DirectoryServer::new(args.data_directory, Some(conn_type))?);

    start_directory_server(directory)?;

    Ok(())
}

use clap::Parser;
use coinswap::{
    market::directory::{start_directory_server, DirectoryServer, DirectoryServerError},
    tor::setup_mitosis,
    utill::{setup_directory_logger, ConnectionType},
};
use std::{path::PathBuf, str::FromStr, sync::Arc};

#[derive(Parser)]
#[clap(version = option_env ! ("CARGO_PKG_VERSION").unwrap_or("unknown"),
author = option_env ! ("CARGO_PKG_AUTHORS").unwrap_or(""))]
struct Cli {
    /// Optional network type.
    #[clap(long, short = 'n', default_value = "tor", possible_values = &["tor", "clearnet"])]
    network: String,
    /// Optional DNS data directory. Default value : "~/.coinswap/dns"
    #[clap(long, short = 'd')]
    data_directory: Option<PathBuf>,
}

fn main() -> Result<(), DirectoryServerError> {
    setup_directory_logger(log::LevelFilter::Info);

    let args = Cli::parse();

    let conn_type = ConnectionType::from_str(&args.network)?;

    if conn_type == ConnectionType::TOR {
        setup_mitosis();
    }

    let directory = Arc::new(DirectoryServer::new(args.data_directory, Some(conn_type))?);

    start_directory_server(directory)?;

    Ok(())
}

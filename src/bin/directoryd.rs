use bitcoind::bitcoincore_rpc::Auth;
use clap::Parser;
use coinswap::{
    market::directory::{start_directory_server, DirectoryServer, DirectoryServerError},
    utill::{parse_proxy_auth, setup_directory_logger, ConnectionType},
    wallet::RPCConfig,
};

use std::{path::PathBuf, sync::Arc};

#[derive(Parser)]
#[clap(version = option_env ! ("CARGO_PKG_VERSION").unwrap_or("unknown"),
author = option_env ! ("CARGO_PKG_AUTHORS").unwrap_or(""))]
struct Cli {
    /// Optional DNS data directory. Default value: "~/.coinswap/dns"
    #[clap(long, short = 'd')]
    data_directory: Option<PathBuf>,
    /// Sets the full node address for rpc connection.
    #[clap(
        name = "ADDRESS:PORT",
        long,
        short = 'r',
        default_value = "127.0.0.1:48332"
    )]
    pub(crate) rpc: String,
    /// Sets the rpc basic authentication.
    #[clap(
        name = "USER:PASSWORD",
        short = 'a',
        long,
        value_parser = parse_proxy_auth,
        default_value = "user:password",
    )]
    pub auth: (String, String),
}

fn main() -> Result<(), DirectoryServerError> {
    let args = Cli::parse();
    setup_directory_logger(log::LevelFilter::Info, args.data_directory.clone());

    let rpc_config = RPCConfig {
        url: args.rpc,
        auth: Auth::UserPass(args.auth.0, args.auth.1),
        wallet_name: "random".to_string(), // we can put anything here as it will get updated in the init.
    };

    #[cfg(not(feature = "integration-test"))]
    let connection_type = ConnectionType::TOR;

    #[cfg(feature = "integration-test")]
    let connection_type = ConnectionType::CLEARNET;

    let directory = Arc::new(DirectoryServer::new(
        args.data_directory,
        Some(connection_type),
    )?);

    start_directory_server(directory, Some(rpc_config))?;

    Ok(())
}

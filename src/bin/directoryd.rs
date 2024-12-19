use bitcoind::bitcoincore_rpc::Auth;
use clap::Parser;
use coinswap::{
    market::directory::{start_directory_server, DirectoryServer, DirectoryServerError},
    tor::setup_mitosis,
    utill::{parse_proxy_auth, setup_directory_logger, ConnectionType},
    wallet::RPCConfig,
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
    /// Sets the full node address for rpc connection.
    #[clap(
        name = "ADDRESS:PORT",
        long,
        short = 'r',
        default_value = "127.0.0.1:18443"
    )]
    pub rpc: String,
    /// Sets the rpc basic authentication.
    #[clap(
        name = "USER:PASSWORD",
        short = 'a',
        long,
        value_parser = parse_proxy_auth,
        default_value = "user:password",
    )]
    pub auth: (String, String),
    /// Sets the full node network, this should match with the network of the running node.
    #[clap(
        name = "rpc_network",
        long,
        default_value = "regtest", possible_values = &["regtest", "signet", "mainnet"]
    )]
    pub rpc_network: String,
    /// Sets the maker wallet's name. If the wallet file already exists at data-directory, it will load that wallet.
    #[clap(name = "WALLET", long, short = 'w')]
    pub wallet_name: Option<String>,
}

fn main() -> Result<(), DirectoryServerError> {
    setup_directory_logger(log::LevelFilter::Info);

    let args = Cli::parse();

    let rpc_network = bitcoin::Network::from_str(&args.rpc_network).unwrap();

    let conn_type = ConnectionType::from_str(&args.network)?;

    let mut rpc_config = RPCConfig {
        url: args.rpc,
        auth: Auth::UserPass(args.auth.0, args.auth.1),
        network: rpc_network,
        wallet_name: "random".to_string(), // we can put anything here as it will get updated in the init.
    };

    if let Some(wallet_name) = args.wallet_name {
        rpc_config.wallet_name = wallet_name;
    }

    if conn_type == ConnectionType::TOR {
        setup_mitosis();
    }

    let directory = Arc::new(DirectoryServer::new(args.data_directory, Some(conn_type))?);

    start_directory_server(directory, Some(rpc_config))?;

    Ok(())
}

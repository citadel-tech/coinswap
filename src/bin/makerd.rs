use bitcoind::bitcoincore_rpc::Auth;
use clap::Parser;
use coinswap::{
    maker::{start_maker_server, Maker, MakerBehavior},
    utill::{
        parse_proxy_auth, read_bitcoin_network_string, read_connection_network_string, setup_logger,
    },
    wallet::RPCConfig,
};
use std::{path::PathBuf, sync::Arc};

/// The Maker Server.
///
/// This app starts the Maker server.
#[derive(Parser)]
#[clap(version = option_env ! ("CARGO_PKG_VERSION").unwrap_or("unknown"),
author = option_env ! ("CARGO_PKG_AUTHORS").unwrap_or(""))]
struct Cli {
    /// Optional Connection Network Type
    #[clap(long, default_value = "clearnet", possible_values = &["tor", "clearnet"])]
    network: String,
    /// Optional DNS data directory. Default value : "~/.coinswap/maker"
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
        name = "USER:PASSWD",
        short = 'a',
        long,
        value_parser = parse_proxy_auth,
        default_value = "user:password",
    )]
    pub auth: (String, String),
    /// Sets the full node network, this should match with the network of the running node.
    #[clap(
        name = "NETWORK",
        long,
        short = 'n',
        default_value = "regtest", possible_values = &["regtest", "signet", "mainnet"]
    )]
    pub rpc_network: String,
    /// Sets the maker wallet's name. If the wallet file already exists at data-directory, it will load that wallet.
    #[clap(name = "WALLET", long, short = 'w', default_value = "maker")]
    pub wallet_name: String,
}

fn main() -> std::io::Result<()> {
    setup_logger();

    let args = Cli::parse();

    let rpc_network = read_bitcoin_network_string(&args.rpc_network).unwrap();

    let conn_type = read_connection_network_string(&args.network).unwrap();

    let rpc_config = RPCConfig {
        url: args.rpc,
        auth: Auth::UserPass(args.auth.0, args.auth.1),
        network: rpc_network,
        wallet_name: args.wallet_name.clone(),
    };

    let maker = Arc::new(
        Maker::init(
            args.data_directory,
            Some(args.wallet_name),
            Some(rpc_config),
            None,
            None,
            None,
            Some(conn_type),
            MakerBehavior::Normal,
        )
        .unwrap(),
    );

    start_maker_server(maker).unwrap();

    Ok(())
}

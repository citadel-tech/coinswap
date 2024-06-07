use bitcoind::bitcoincore_rpc::Auth;
use clap::Parser;
use std::{
    path::PathBuf,
    sync::{Arc, RwLock},
};

use coinswap::{
    taker::{rpc::start_taker_rpc_server, SwapParams, Taker, TakerBehavior},
    utill::{
        parse_proxy_auth, read_bitcoin_network_string, read_connection_network_string, setup_logger,
    },
    wallet::RPCConfig,
};

#[derive(Parser, Debug)]
#[clap(version = option_env ! ("CARGO_PKG_VERSION").unwrap_or("unknown"),
author = option_env ! ("CARGO_PKG_AUTHORS").unwrap_or(""))]
struct Cli {
    #[clap(long, default_value = "clearnet",possible_values = &["tor","clearnet"])]
    network: String,
    #[clap(long, short = 'd')]
    data_directory: Option<PathBuf>,
    #[clap(
        name = "ADDRESS:PORT",
        long,
        short = 'r',
        default_value = "127.0.0.1:18443"
    )]
    pub rpc: String,
    #[clap(name="USER:PASSWORD",short='a',long, value_parser = parse_proxy_auth, default_value = "user:password")]
    pub auth: (String, String),
    #[clap(
        name = "NETWORK",
        long,
        short = 'n',
        default_value = "regtest", possible_values = &["regtest", "signet", "mainnet"]
    )]
    pub rpc_network: String,
    #[clap(name = "WALLET", long, short = 'w', default_value = "taker")]
    pub wallet_name: String,
    #[clap(name = "maker_count", default_value = "2")]
    pub maker_count: u16,
    #[clap(name = "send_amount", default_value = "500000")]
    pub send_amount: u64,
    #[clap(name = "tx_count", default_value = "3")]
    pub tx_count: u32,
    #[clap(name = "fee_rate", default_value = "1000")]
    pub fee_rate: u64,
    #[clap(name = "required_confirms", default_value = "1000")]
    pub required_confirms: u64,
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

    let swap_params = SwapParams {
        send_amount: args.send_amount,
        maker_count: args.maker_count,
        tx_count: args.tx_count,
        required_confirms: args.required_confirms,
        fee_rate: args.fee_rate,
    };

    let taker = Arc::new(RwLock::new(
        Taker::init(
            args.data_directory,
            Some(args.wallet_name),
            Some(rpc_config),
            TakerBehavior::Normal,
            Some(conn_type),
        )
        .unwrap(),
    ));
    start_taker_rpc_server(taker, rpc_network, swap_params);
    Ok(())
}

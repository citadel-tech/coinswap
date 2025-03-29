use bitcoind::bitcoincore_rpc::Auth;
use clap::Parser;
use coinswap::{
    maker::{start_maker_server, Maker, MakerBehavior, MakerError},
    utill::{parse_proxy_auth, setup_maker_logger, ConnectionType},
    wallet::RPCConfig,
};
use std::{path::PathBuf, sync::Arc};
/// Coinswap Maker Server
///
/// The server requires a Bitcoin Core RPC connection running in Testnet4. It requires some starting balance, around 50,000 sats for Fidelity + Swap Liquidity (suggested 50,000 sats).
/// So topup with at least 0.001 BTC to start all the node processses. Suggested faucet: https://mempool.space/testnet4/faucet
///
/// All server process will start after the fidelity bond transaction confirms. This may take some time. Approx: 10 mins.
/// Once the bond confirms, the server starts listening for incoming swap requests. As it performs swaps for clients, it keeps earning fees.
///
/// The server is operated with the maker-cli app, for all basic wallet related operations.
///
/// For more detailed usage information, please refer: https://github.com/citadel-tech/coinswap/blob/master/docs/app%20demos/makerd.md
///
/// This is early beta, and there are known and unknown bugs. Please report issues at: https://github.com/citadel-tech/coinswap/issues
#[derive(Parser, Debug)]
#[clap(version = option_env ! ("CARGO_PKG_VERSION").unwrap_or("unknown"),
author = option_env ! ("CARGO_PKG_AUTHORS").unwrap_or(""))]
struct Cli {
    /// Optional DNS data directory. Default value : "~/.coinswap/maker"
    #[clap(long, short = 'd')]
    data_directory: Option<PathBuf>,
    /// Bitcoin Core  RPC network address.
    #[clap(
        name = "ADDRESS:PORT",
        long,
        short = 'r',
        default_value = "127.0.0.1:48332"
    )]
    pub rpc: String,
    /// Bitcoin Core RPC authentication string (username, password).
    #[clap(
        name = "USER:PASSWORD",
        short = 'a',
        long,
        value_parser = parse_proxy_auth,
        default_value = "user:password",
    )]
    pub auth: (String, String),
    #[clap(long, short = 't', default_value = "")]
    pub tor_auth: String,
    /// Optional wallet name. If the wallet exists, load the wallet, else create a new wallet with given name. Default: maker-wallet
    #[clap(name = "WALLET", long, short = 'w')]
    pub(crate) wallet_name: Option<String>,
}

fn main() -> Result<(), MakerError> {
    let args = Cli::parse();
    setup_maker_logger(log::LevelFilter::Info, args.data_directory.clone());

    let rpc_config = RPCConfig {
        url: args.rpc,
        auth: Auth::UserPass(args.auth.0, args.auth.1),
        wallet_name: "random".to_string(), // we can put anything here as it will get updated in the init.
    };

    #[cfg(not(feature = "integration-test"))]
    let connection_type = ConnectionType::TOR;

    #[cfg(feature = "integration-test")]
    let connection_type = ConnectionType::CLEARNET;

    let maker = Arc::new(Maker::init(
        args.data_directory,
        args.wallet_name,
        Some(rpc_config),
        None,
        None,
        None,
        Some(args.tor_auth),
        None,
        Some(connection_type),
        MakerBehavior::Normal,
    )?);

    start_maker_server(maker)?;

    Ok(())
}

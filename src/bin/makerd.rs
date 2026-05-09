use bitcoind::bitcoincore_rpc::Auth;
use clap::Parser;
use coinswap::{
    maker::{bind_port_retry, start_server, MakerError, MakerServer, MakerServerConfig},
    utill::{parse_proxy_auth, setup_maker_logger},
    wallet::RPCConfig,
};
use std::{path::PathBuf, sync::Arc};

/// Coinswap Maker Server
///
/// The server requires a Bitcoin Core RPC connection running in Testnet4. It requires some starting balance, around 50,000 sats for Fidelity + Swap Liquidity (suggested 50,000 sats).
/// So topup with at least 0.001 BTC to start all the node processses. Suggested [faucet here]<https://mempool.space/testnet4/faucet>
///
/// All server processes will start after the fidelity bond transaction is confirmed. This may take some time. Approx: 10 mins.
/// Once the bond is confirmed, the server starts listening for incoming swap requests. As it performs swaps for clients, it keeps earning fees.
///
/// The server is operated with the maker-cli app, for all basic wallet related operations.
///
/// For more detailed usage information, please refer the [Maker Doc]<https://github.com/citadel-tech/coinswap/blob/master/docs/makerd.md>
///
/// This is early beta, and there are known and unknown bugs. Please report issues in the [Project Issue Board]<https://github.com/citadel-tech/coinswap/issues>
#[derive(Parser, Debug)]
#[clap(version = option_env ! ("CARGO_PKG_VERSION").unwrap_or("unknown"),
author = option_env ! ("CARGO_PKG_AUTHORS").unwrap_or(""))]
struct Cli {
    /// Optional DNS data directory. Default value: "~/.coinswap/maker"
    #[clap(long, short = 'd')]
    data_directory: Option<PathBuf>,
    /// Bitcoin Core RPC network address.
    #[clap(
        name = "ADDRESS:PORT",
        long,
        short = 'r',
        default_value = "127.0.0.1:38332"
    )]
    pub rpc: String,
    /// Bitcoin Core ZMQ address:port value
    #[clap(
        name = "ZMQ",
        long,
        short = 'z',
        default_value = "tcp://127.0.0.1:28332"
    )]
    pub zmq: String,
    /// Bitcoin Core RPC authentication string (username, password).
    #[clap(
        name = "USER:PASSWORD",
        short = 'a',
        long,
        value_parser = parse_proxy_auth,
        default_value = "user:password",
    )]
    pub auth: (String, String),
    #[clap(long, short = 't')]
    pub tor_auth: Option<String>,
    /// Optional wallet name. If the wallet exists, load the wallet, else create a new wallet with the given name. Default: maker-wallet
    #[clap(name = "WALLET", long, short = 'w')]
    pub(crate) wallet_name: Option<String>,
    /// Optional Password for the encryption of the wallet.
    #[clap(name = "PASSWORD", long, short = 'p')]
    pub password: Option<String>,
    /// When enabled (and built with `--features 'hotpath hotpath-alloc'`), this will:
    /// - write JSON reports under `{data_dir}/hotpath/`
    /// - print timing + alloc tables when each swap completes
    #[cfg(feature = "hotpath")]
    #[clap(long)]
    pub hotpath: bool,
}

fn main() -> Result<(), MakerError> {
    let args = Cli::parse();

    setup_maker_logger(log::LevelFilter::Info, args.data_directory.clone());

    let data_dir = args
        .data_directory
        .unwrap_or_else(coinswap::utill::get_maker_dir);

    // Start Hotpath at process startup
    #[cfg(feature = "hotpath")]
    if args.hotpath {
        let report_path =
            coinswap::hotpath_local::default_report_path(&data_dir, "makerd_run", "session");

        let run = coinswap::hotpath_local::HotpathRun::start("coinswap_maker_swap", report_path)
            .map_err(MakerError::IO)?;

        if !coinswap::hotpath_local::try_set_process_hotpath_run(run) {
            log::warn!("Hotpath run already stored; startup profiling disabled");
        }
    }

    // Load static settings from config file (auto-creates defaults if missing)
    let config_path = data_dir.join("config.toml");
    let mut config = MakerServerConfig::new(Some(&config_path))?;

    // Override with CLI / runtime args
    config.data_dir = data_dir;
    config.wallet_name = args
        .wallet_name
        .unwrap_or_else(|| "maker-wallet".to_string());
    config.rpc_config = RPCConfig {
        url: args.rpc,
        auth: Auth::UserPass(args.auth.0, args.auth.1),
        wallet_name: "random".to_string(), // updated during init
    };
    config.zmq_addr = args.zmq;
    config.password = args.password;
    if let Some(tor_auth) = args.tor_auth {
        config.tor_auth_password = tor_auth;
    }

    // First run: discover available port and save to config
    const DEFAULT_NETWORK_PORT: u16 = 6102;
    if config.network_port == DEFAULT_NETWORK_PORT {
        let (_, port) = bind_port_retry(config.network_port)?;
        config.network_port = port;
        config.write_to_file(&config_path)?;
    }

    // Discover and save RPC port to config
    let (_, rpc_port) = bind_port_retry(config.rpc_port - 2)?;
    config.rpc_port = rpc_port;
    config.write_to_file(&config_path)?;

    let maker = Arc::new(MakerServer::init(config)?);
    start_server(maker)?;

    Ok(())
}

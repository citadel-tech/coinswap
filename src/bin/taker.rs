use std::path::PathBuf;

use bitcoind::bitcoincore_rpc::{json::ListUnspentResultEntry, Auth};
use clap::Parser;
use coinswap::{
    taker::{SwapParams, Taker, TakerBehavior},
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
    #[clap(subcommand)]
    command: Commands,
}

#[derive(Parser, Debug)]
enum Commands {
    SeedUtxo,
    SwapUtxo,
    ContractUtxo,
    FidelityUtxo,
    SeedBalance,
    SwapBalance,
    ContractBalance,
    FidelityBalance,
    TotalBalance,
    GetNewAddress,
    SyncOfferBook,
    DoCoinswap,
}

fn main() {
    setup_logger();
    let args = Cli::parse();
    let rpc_network = read_bitcoin_network_string(&args.rpc_network).unwrap();
    let connection_type = read_connection_network_string(&args.network).unwrap();
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

    let mut taker = Taker::init(
        args.data_directory.clone(),
        Some(args.wallet_name.clone()),
        Some(rpc_config.clone()),
        TakerBehavior::Normal,
        Some(connection_type),
    )
    .unwrap();

    match args.command {
        Commands::SeedUtxo => {
            let utxos: Vec<ListUnspentResultEntry> = taker
                .get_wallet()
                .list_live_contract_spend_info(None)
                .unwrap()
                .iter()
                .map(|(l, _)| l.clone())
                .collect();
            println!("{:?}", utxos);
        }
        Commands::SwapUtxo => {
            let utxos: Vec<ListUnspentResultEntry> = taker
                .get_wallet()
                .list_swap_coin_utxo_spend_info(None)
                .unwrap()
                .iter()
                .map(|(l, _)| l.clone())
                .collect();
            println!("{:?}", utxos);
        }
        Commands::ContractUtxo => {
            let utxos: Vec<ListUnspentResultEntry> = taker
                .get_wallet()
                .list_live_contract_spend_info(None)
                .unwrap()
                .iter()
                .map(|(l, _)| l.clone())
                .collect();
            println!("{:?}", utxos);
        }
        Commands::FidelityUtxo => {
            let utxos: Vec<ListUnspentResultEntry> = taker
                .get_wallet()
                .list_fidelity_spend_info(None)
                .unwrap()
                .iter()
                .map(|(l, _)| l.clone())
                .collect();
            println!("{:?}", utxos);
        }
        Commands::ContractBalance => {
            let balance = taker.get_wallet().balance_live_contract(None).unwrap();
            println!("{:?}", balance);
        }
        Commands::SwapBalance => {
            let balance = taker.get_wallet().balance_swap_coins(None).unwrap();
            println!("{:?}", balance);
        }
        Commands::SeedBalance => {
            let balance = taker.get_wallet().balance_descriptor_utxo(None).unwrap();
            println!("{:?}", balance);
        }
        Commands::FidelityBalance => {
            let balance = taker.get_wallet().balance_fidelity_bonds(None).unwrap();
            println!("{:?}", balance);
        }
        Commands::TotalBalance => {
            let balance = taker.get_wallet().balance().unwrap();
            println!("{:?}", balance);
        }
        Commands::GetNewAddress => {
            let address = taker.get_wallet_mut().get_next_external_address().unwrap();
            println!("{:?}", address);
        }
        Commands::SyncOfferBook => {
            let taker2 = Taker::init(
                args.data_directory,
                Some(args.wallet_name),
                Some(rpc_config),
                TakerBehavior::Normal,
                Some(connection_type),
            )
            .unwrap();
            let config = taker2.config.clone();
            let _ = futures::executor::block_on(taker.sync_offerbook(
                read_bitcoin_network_string(&args.network).unwrap(),
                &config,
                args.maker_count,
            ));
        }
        Commands::DoCoinswap => {
            let _ = taker.do_coinswap(swap_params);
        }
    }
}

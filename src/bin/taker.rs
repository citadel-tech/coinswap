use bitcoin::{Address, Amount};
use bitcoind::bitcoincore_rpc::{json::ListUnspentResultEntry, Auth};
use clap::Parser;
use coinswap::{
    taker::{error::TakerError, SwapParams, Taker, TakerBehavior},
    utill::{parse_proxy_auth, setup_taker_logger, ConnectionType},
    wallet::{Destination, RPCConfig, SendAmount},
};
use log::LevelFilter;
use std::{path::PathBuf, str::FromStr};

/// taker-cli is a command line app to use taker client API's.
#[derive(Parser, Debug)]
#[clap(version = option_env ! ("CARGO_PKG_VERSION").unwrap_or("unknown"),
author = option_env ! ("CARGO_PKG_AUTHORS").unwrap_or(""))]
struct Cli {
    /// Optional Connection Network Type
    #[clap(long, default_value = "clearnet",short= 'c', possible_values = &["tor","clearnet"])]
    connection_type: String,
    /// Optional DNS data directory. Default value : "~/.coinswap/taker"
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
    #[clap(name="USER:PASSWORD",short='a',long, value_parser = parse_proxy_auth, default_value = "user:password")]
    pub auth: (String, String),
    /// Sets the full node network, this should match with the network of the running node.
    #[clap(
        long,
        short = 'b',
        default_value = "regtest", possible_values = &["regtest", "signet"]
    )]
    pub bitcoin_network: String,
    /// Sets the taker wallet's name. If the wallet file already exists at data-directory, it will load that wallet.
    #[clap(name = "WALLET", long, short = 'w')]
    pub wallet_name: Option<String>,
    /// Sets the verbosity level of logs.
    /// Default: Determined by the command passed.
    #[clap(long, short = 'v', possible_values = &["off", "error", "warn", "info", "debug", "trace"])]
    pub verbosity: Option<String>,

    /// List of sub commands to process various endpoints of taker cli app.
    #[clap(subcommand)]
    command: Commands,
}

#[derive(Parser, Debug)]
enum Commands {
    /// Returns a list of seed utxos
    SeedUtxo,
    /// Returns a list of swap coin utxos
    SwapUtxo,
    /// Returns a list of live contract utxos
    ContractUtxo,
    /// Returns the total seed balance
    SeedBalance,
    /// Returns the total swap coin balance
    SwapBalance,
    /// Returns the total live contract balance
    ContractBalance,
    /// Returns the total balance of taker wallet
    TotalBalance,
    /// Returns a new address
    GetNewAddress,
    /// Send to an external wallet address.
    SendToAddress {
        #[clap(name = "address")]
        address: String,
        /// Amount to be sent (in sats)
        #[clap(name = "amount")]
        amount: u64,
        /// Fee of a Tx(in sats)
        #[clap(name = "fee")]
        fee: u64,
    },

    /// List all offers in taker's offerbook
    ListOffers,
    /// Initiate the coinswap process
    DoCoinswap {
        /// Sets the send amount.
        #[clap(name = "send_amount")]
        send_amount: u64,
        /// Sets the fee-rate.
        #[clap(name = "fee_rate")]
        fee_rate: u64,
        /// Sets the required on-chain confirmations.
        #[clap(name = "required_confirms")] // TODO: Should we have default as 1?
        required_confirms: u64,
        /// Sets the maker count to initiate coinswap with.
        #[clap(name = "maker_count", default_value = "2")]
        // THINK: I do not want to  discard its default value as `maker_count =2`` resembles the coinswap protocol example.
        // But due to which this arg has to kept after the required ones -> so will it makes sense to have this arg at the end even though
        // It's the most important aspect of `do-coinswap`?
        maker_count: usize,
        /// Sets the transaction count.
        #[clap(name = "tx_count", default_value = "3")]
        tx_count: u32,
    },
}

fn main() -> Result<(), TakerError> {
    let args = Cli::parse();

    let rpc_network = bitcoin::Network::from_str(&args.bitcoin_network).unwrap();
    let connection_type = ConnectionType::from_str(&args.connection_type)?;

    let rpc_config = RPCConfig {
        url: args.rpc,
        auth: Auth::UserPass(args.auth.0, args.auth.1),
        network: rpc_network,
        wallet_name: "random".to_string(), // we can put anything here as it will get updated in the init.
    };

    let mut taker = Taker::init(
        args.data_directory.clone(),
        args.wallet_name.clone(),
        Some(rpc_config.clone()),
        TakerBehavior::Normal,
        Some(connection_type),
    )?;

    // Determines the log level based on the verbosity argument or the command.
    //
    // If verbosity is provided, it converts the string to a `LevelFilter`.
    // Otherwise, the log level is set based on the command.
    let log_level = match args.verbosity {
        Some(level) => LevelFilter::from_str(&level).unwrap(),
        None => match args.command {
            Commands::DoCoinswap { .. } | Commands::SendToAddress { .. } => log::LevelFilter::Info,
            _ => log::LevelFilter::Off,
        },
    };

    setup_taker_logger(log_level);

    match args.command {
        Commands::SeedUtxo => {
            let utxos: Vec<ListUnspentResultEntry> = taker
                .get_wallet()
                .list_descriptor_utxo_spend_info(None)?
                .iter()
                .map(|(l, _)| l.clone())
                .collect();
            println!("{:?}", utxos);
        }
        Commands::SwapUtxo => {
            let utxos: Vec<ListUnspentResultEntry> = taker
                .get_wallet()
                .list_swap_coin_utxo_spend_info(None)?
                .iter()
                .map(|(l, _)| l.clone())
                .collect();
            println!("{:?}", utxos);
        }
        Commands::ContractUtxo => {
            let utxos: Vec<ListUnspentResultEntry> = taker
                .get_wallet()
                .list_live_contract_spend_info(None)?
                .iter()
                .map(|(l, _)| l.clone())
                .collect();
            println!("{:?}", utxos);
        }
        Commands::ContractBalance => {
            let balance = taker.get_wallet().balance_live_contract(None)?;
            println!("{:?}", balance);
        }
        Commands::SwapBalance => {
            let balance = taker.get_wallet().balance_swap_coins(None)?;
            println!("{:?}", balance);
        }
        Commands::SeedBalance => {
            let balance = taker.get_wallet().balance_descriptor_utxo(None)?;
            println!("{:?}", balance);
        }
        Commands::TotalBalance => {
            let balance = taker.get_wallet().balance()?;
            println!("{:?}", balance);
        }
        Commands::GetNewAddress => {
            let address = taker.get_wallet_mut().get_next_external_address()?;
            println!("{:?}", address);
        }
        Commands::SendToAddress {
            address,
            amount,
            fee,
        } => {
            // NOTE:
            //
            // Currently, we take `fee` instead of `fee_rate` because we cannot calculate the fee for a
            // transaction that hasn't been created yet when only a `fee_rate` is provided.
            //
            // As a result, the user must supply the fee as a parameter, and the function will return the
            // transaction hex and the calculated `fee_rate`.
            // This allows the user to infer what fee is needed for a successful transaction.
            //
            // This approach will be improved in the future BDK integration.

            let fee = Amount::from_sat(fee);

            let amount = Amount::from_sat(amount);

            let coins_to_spend = taker.get_wallet().coin_select(amount + fee)?;

            let destination =
                Destination::Address(Address::from_str(&address).unwrap().assume_checked());

            let tx = taker.get_wallet_mut().spend_from_wallet(
                fee,
                SendAmount::Amount(amount),
                destination,
                &coins_to_spend,
            )?;

            // Derive fee rate from given `fee` argument.
            let calculated_fee_rate = fee / (tx.weight());

            println!(
                "transaction_hex :  {:?}",
                bitcoin::consensus::encode::serialize_hex(&tx)
            );
            println!("Calculated FeeRate : {:#}", calculated_fee_rate);
        }

        Commands::ListOffers => {
            let offers = taker.list_offers();
            println!("{:?}", offers);
        }
        Commands::DoCoinswap {
            maker_count,
            send_amount,
            fee_rate,
            required_confirms,
            tx_count,
        } => {
            let swap_params = SwapParams {
                send_amount: Amount::from_sat(send_amount),
                maker_count,
                tx_count,
                required_confirms,
                fee_rate: Amount::from_sat(fee_rate),
            };
            taker.do_coinswap(swap_params)?;
        }
    }

    Ok(())
}

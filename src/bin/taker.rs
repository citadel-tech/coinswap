use bitcoin::{Address, Amount};
use bitcoind::bitcoincore_rpc::Auth;
use clap::Parser;
use coinswap::{
    taker::{error::TakerError, SwapParams, Taker, TakerBehavior},
    utill::{
        parse_proxy_auth, setup_taker_logger, ConnectionType, DEFAULT_TX_FEE_RATE,
        REQUIRED_CONFIRMS, UTXO,
    },
    wallet::{Destination, RPCConfig},
};
use log::LevelFilter;
use serde_json::{json, to_string_pretty};
use std::{path::PathBuf, str::FromStr};
/// A simple command line app to operate as coinswap client.
///
/// The app works as a regular Bitcoin wallet with the added capability to perform coinswaps. The app
/// requires a running Bitcoin Core node with RPC access. It currently only runs on Testnet4.
/// Suggested faucet for getting Testnet4 coins: <https://mempool.space/testnet4/faucet>
///
/// For more detailed usage information, please refer: <https://github.com/citadel-tech/coinswap/blob/master/docs/app%20demos/taker.md>
///
/// This is early beta, and there are known and unknown bugs. Please report issues at: <https://github.com/citadel-tech/coinswap/issues>
#[derive(Parser, Debug)]
#[clap(version = option_env ! ("CARGO_PKG_VERSION").unwrap_or("unknown"),
author = option_env ! ("CARGO_PKG_AUTHORS").unwrap_or(""))]
struct Cli {
    /// Optional data directory. Default value: "~/.coinswap/taker"
    #[clap(long, short = 'd')]
    data_directory: Option<PathBuf>,

    /// Bitcoin Core RPC address:port value
    #[clap(
        name = "ADDRESS:PORT",
        long,
        short = 'r',
        default_value = "127.0.0.1:48332"
    )]
    pub rpc: String,

    /// Bitcoin Core RPC authentication string. Ex: username:password
    #[clap(name="USER:PASSWORD",short='a',long, value_parser = parse_proxy_auth, default_value = "user:password")]
    pub auth: (String, String),
    #[clap(long, short = 't', default_value = "")]
    pub tor_auth: String,

    /// Sets the taker wallet's name. If the wallet file already exists, it will load that wallet. Default: taker-wallet
    #[clap(name = "WALLET", long, short = 'w')]
    pub wallet_name: Option<String>,

    /// Sets the verbosity level of debug.log file
    #[clap(long, short = 'v', possible_values = &["off", "error", "warn", "info", "debug", "trace"], default_value = "info")]
    pub verbosity: String,

    /// List of commands for various wallet operations
    #[clap(subcommand)]
    command: Commands,
}

#[derive(Parser, Debug)]
enum Commands {
    // TODO: Design a better structure to display different utxos and balance groups.
    /// Lists all utxos we know about along with their spend info. This is useful for debugging
    ListUtxo,
    /// Lists all single signature wallet Utxos. These are all non-swap regular wallet utxos.
    ListUtxoRegular,
    /// Lists all utxos received in incoming swaps
    ListUtxoSwap,
    /// Lists all utxos that we need to claim via timelock. If you see entries in this list, do a `taker recover` to claim them.
    ListUtxoContract,
    /// Get total wallet balances of different categories.
    /// regular: All single signature regular wallet coins (seed balance).
    /// swap: All 2of2 multisig coins received in swaps.
    /// contract: All live contract transaction balance locked in timelocks. If you see value in this field, you have unfinished or malfinished swaps. You can claim them back with the recover command.
    /// spendable: Spendable amount in wallet (regular + swap balance).
    GetBalances,
    /// Returns a new address
    GetNewAddress,
    /// Send to an external wallet address.
    SendToAddress {
        /// Recipient's address.
        #[clap(long, short = 't')]
        address: String,
        /// Amount to send in sats
        #[clap(long, short = 'a')]
        amount: u64,
        /// Feerate in sats/vByte. Defaults to 2 sats/vByte
        #[clap(long, short = 'f')]
        feerate: Option<f64>,
    },
    /// Update the offerbook with current market offers and display them
    FetchOffers,

    // TODO: Also add ListOffers command to just list the current book.
    /// Initiate the coinswap process
    Coinswap {
        /// Sets the maker count to swap with. Swapping with less than 2 makers is not allowed to maintain client privacy.
        /// Adding more makers in the swap will incur more swap fees.
        #[clap(long, short = 'm', default_value = "2")]
        makers: usize,
        /// Sets the swap amount in sats.
        #[clap(long, short = 'a', default_value = "20000")]
        amount: u64,
        // /// Sets how many new swap utxos to get. The swap amount will be randomly distributed across the new utxos.
        // /// Increasing this number also increases the total swap fee.
        // #[clap(long, short = 'u', default_value = "1")]
        // utxos: u32,
    },
    /// Recover from all failed swaps
    Recover,
}

fn main() -> Result<(), TakerError> {
    let args = Cli::parse();
    setup_taker_logger(
        LevelFilter::from_str(&args.verbosity).unwrap(),
        matches!(
            args.command,
            Commands::Recover | Commands::FetchOffers | Commands::Coinswap { .. }
        ),
        args.data_directory.clone(), // default path handled inside the function.
    );

    let rpc_config = RPCConfig {
        url: args.rpc,
        auth: Auth::UserPass(args.auth.0, args.auth.1),
        wallet_name: "random".to_string(), // we can put anything here as it will get updated in the init.
    };

    #[cfg(not(feature = "integration-test"))]
    let connection_type = ConnectionType::TOR;

    #[cfg(feature = "integration-test")]
    let connection_type = ConnectionType::CLEARNET;

    let mut taker = Taker::init(
        args.data_directory.clone(),
        args.wallet_name.clone(),
        Some(rpc_config.clone()),
        TakerBehavior::Normal,
        None,
        Some(args.tor_auth),
        Some(connection_type),
    )?;

    match args.command {
        Commands::ListUtxo => {
            let utxos = taker.get_wallet().list_all_utxo_spend_info()?;
            for utxo in utxos {
                let utxo = UTXO::from_utxo_data(utxo);
                println!("{}", serde_json::to_string_pretty(&utxo)?);
            }
        }
        Commands::ListUtxoRegular => {
            let utxos = taker.get_wallet().list_descriptor_utxo_spend_info()?;
            for utxo in utxos {
                let utxo = UTXO::from_utxo_data(utxo);
                println!("{}", serde_json::to_string_pretty(&utxo)?);
            }
        }
        Commands::ListUtxoSwap => {
            let utxos = taker
                .get_wallet()
                .list_incoming_swap_coin_utxo_spend_info()?;
            for utxo in utxos {
                let utxo = UTXO::from_utxo_data(utxo);
                println!("{}", serde_json::to_string_pretty(&utxo)?);
            }
        }
        Commands::ListUtxoContract => {
            let utxos = taker
                .get_wallet()
                .list_live_timelock_contract_spend_info()?;
            for utxo in utxos {
                let utxo = UTXO::from_utxo_data(utxo);
                println!("{}", serde_json::to_string_pretty(&utxo)?);
            }
        }
        Commands::GetBalances => {
            let balances = taker.get_wallet().get_balances()?;
            println!(
                "{}",
                to_string_pretty(&json!({
                    "regular": balances.regular.to_sat(),
                    "contract": balances.contract.to_sat(),
                    "swap": balances.swap.to_sat(),
                    "spendable": balances.spendable.to_sat(),
                }))
                .unwrap()
            );
        }
        Commands::GetNewAddress => {
            let address = taker.get_wallet_mut().get_next_external_address()?;
            println!("{address:?}");
        }
        Commands::SendToAddress {
            address,
            amount,
            feerate,
        } => {
            let amount = Amount::from_sat(amount);

            let coins_to_spend = taker
                .get_wallet_mut()
                .coin_select(amount, feerate.unwrap_or(DEFAULT_TX_FEE_RATE))?;

            let destination = Destination::Multi(vec![(
                Address::from_str(&address).unwrap().assume_checked(),
                amount,
            )]);

            let tx = taker.get_wallet_mut().spend_from_wallet(
                feerate.unwrap_or(DEFAULT_TX_FEE_RATE),
                destination,
                &coins_to_spend,
            )?;

            let txid = taker.get_wallet().send_tx(&tx).unwrap();

            println!("{txid}");

            taker.get_wallet_mut().sync_no_fail();
        }

        Commands::FetchOffers => {
            let all_offers = {
                let offerbook = taker.fetch_offers()?;
                offerbook
                    .all_makers()
                    .iter()
                    .cloned()
                    .cloned()
                    .collect::<Vec<_>>()
            };
            if all_offers.is_empty() {
                println!("NO LIVE OFFERS FOUND!! You should run a maker!!");
                return Ok(());
            } else {
                all_offers.iter().try_for_each(|offer| {
                    println!("{}", taker.display_offer(offer)?);
                    Ok::<_, TakerError>(())
                })?;
            }
        }
        Commands::Coinswap { makers, amount } => {
            let swap_params = SwapParams {
                send_amount: Amount::from_sat(amount),
                maker_count: makers,
                tx_count: 1,
                required_confirms: REQUIRED_CONFIRMS,
            };
            taker.do_coinswap(swap_params)?;
        }

        Commands::Recover => {
            taker.recover_from_swap()?;
        }
    }

    Ok(())
}

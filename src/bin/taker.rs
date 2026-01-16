use bitcoin::{Address, Amount};
use bitcoind::bitcoincore_rpc::Auth;
use clap::Parser;
use coinswap::{
    taker::{error::TakerError, offers::MakerState, SwapParams, Taker, TaprootTaker},
    utill::{parse_proxy_auth, setup_taker_logger, MIN_FEE_RATE, UTXO},
    wallet::{AddressType, Destination, RPCConfig, Wallet},
};
use log::LevelFilter;
use serde_json::{json, to_string_pretty};
use std::{path::PathBuf, str::FromStr};

#[cfg(feature = "integration-test")]
use coinswap::taker::api2::TakerBehavior as TaprootTakerBehavior;
#[cfg(feature = "integration-test")]
use coinswap::taker::TakerBehavior;

/// A simple command line app to operate as coinswap client.
///
/// The app works as a regular Bitcoin wallet with the added capability to perform coinswaps. The app
/// requires a running Bitcoin Core node with RPC access. It currently only runs on Testnet4.
/// Suggested faucet for getting Signet coins (tor browser required): <http://s2ncekhezyo2tkwtftti3aiukfpqmxidatjrdqmwie6xnf2dfggyscad.onion/>
///
/// For more detailed usage information, please refer: <https://github.com/citadel-tech/coinswap/blob/master/docs/taker.md>
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

    /// Bitcoin Core RPC authentication string. Ex: username:password
    #[clap(name="USER:PASSWORD",short='a',long, value_parser = parse_proxy_auth, default_value = "user:password")]
    pub auth: (String, String),
    #[clap(long, short = 't')]
    pub tor_auth: Option<String>,

    /// Sets the taker wallet's name. If the wallet file already exists, it will load that wallet. Default: taker-wallet
    #[clap(name = "WALLET", long, short = 'w')]
    pub wallet_name: Option<String>,

    /// Optional Password for the encryption of the wallet.
    #[clap(name = "PASSWORD", long, short = 'p')]
    pub password: Option<String>,

    /// Sets the verbosity level of debug.log file
    #[clap(long, short = 'v', possible_values = &["off", "error", "warn", "info", "debug", "trace"], default_value = "info")]
    pub verbosity: String,

    /// Use experimental Taproot-based coinswap protocol
    #[clap(long)]
    pub taproot: bool,

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

    /// Backup the selected wallet.
    ///
    /// You can specify a custom wallet using the default `-w, --WALLET` parameter:
    ///
    /// -w, --wallet_name WALLET-NAME
    ///
    /// The backup will be created in the current working directory with the filename:
    /// `<wallet_name>-backup.json`.
    ///
    /// Use the `-e, --encrypt` flag to encrypt the backup. If enabled, you will be prompted
    /// interactively to enter a passphrase.
    ///
    ///
    #[clap(verbatim_doc_comment)]
    Backup {
        #[clap(long, short = 'e')]
        encrypt: bool,
    },

    /// Restore a wallet from a backup file.
    ///
    /// The `-f, --backup-file <FILE>` parameter specifies the backup file to restore from.
    ///
    /// You can optionally specify a wallet name using the default `-w, --WALLET` parameter.
    /// If no wallet name is provided, the wallet will be restored with its original name
    /// stored in the backup. If a wallet name is provided, the backup will be restored
    /// under that name instead.
    Restore {
        #[clap(long, short = 'f')]
        backup_file: String,
    },
}

fn main() -> Result<(), TakerError> {
    let args = Cli::parse();
    setup_taker_logger(
        LevelFilter::from_str(&args.verbosity).unwrap(),
        matches!(
            args.command,
            Commands::Recover
                | Commands::FetchOffers
                | Commands::Backup { .. }
                | Commands::Restore { .. }
                | Commands::Coinswap { .. }
        ),
        args.data_directory.clone(), // default path handled inside the function.
    );

    let rpc_config = RPCConfig {
        url: args.rpc,
        auth: Auth::UserPass(args.auth.0, args.auth.1),
        wallet_name: "random_1".to_string(), // we can put anything here as it will get updated in the init.
    };

    match &args.command {
        Commands::Restore { backup_file } => {
            Taker::restore_wallet(
                args.data_directory,
                args.wallet_name,
                Some(rpc_config.clone()),
                backup_file,
            );
        }
        Commands::Recover if args.taproot => {
            log::warn!("Using experimental Taproot-based recovery");
            let mut taproot_taker = TaprootTaker::init(
                args.data_directory.clone(),
                args.wallet_name.clone(),
                Some(rpc_config.clone()),
                None,
                args.tor_auth,
                args.zmq,
                None,
                #[cfg(feature = "integration-test")]
                TaprootTakerBehavior::Normal,
            )?;
            taproot_taker.sync_wallet()?;
            taproot_taker.recover_from_swap()?;
        }
        Commands::Coinswap { makers, amount } if args.taproot => {
            // For taproot coinswap, skip regular Taker initialization
            log::warn!("Using experimental Taproot-based coinswap protocol");
            let mut taproot_taker = TaprootTaker::init(
                args.data_directory.clone(),
                args.wallet_name.clone(),
                Some(rpc_config.clone()),
                None,
                args.tor_auth,
                args.zmq,
                None,
                #[cfg(feature = "integration-test")]
                TaprootTakerBehavior::Normal,
            )?;

            let taproot_swap_params = coinswap::taker::api2::SwapParams {
                send_amount: Amount::from_sat(*amount),
                maker_count: *makers,
                tx_count: 1,
                required_confirms: 1,
                manually_selected_outpoints: None,
            };
            taproot_taker.do_coinswap(taproot_swap_params)?;
        }
        _ => {
            // Only initialize Taker if the command is NOT Restore, taproot Recover, or taproot Coinswap.
            // For Restore, we don't initialize Taker because it tries to load the wallet,
            // which may not exist yet before restoring from the backup.

            let mut taker = Taker::init(
                args.data_directory.clone(),
                args.wallet_name.clone(),
                Some(rpc_config.clone()),
                #[cfg(feature = "integration-test")]
                TakerBehavior::Normal,
                None,
                args.tor_auth,
                args.zmq,
                args.password,
            )?;
            taker.sync_wallet()?;
            match &args.command {
                Commands::ListUtxo => {
                    let utxos = taker.get_wallet().list_all_utxo_spend_info();
                    for utxo in utxos {
                        let utxo = UTXO::from_utxo_data(utxo);
                        println!("{}", serde_json::to_string_pretty(&utxo)?);
                    }
                }
                Commands::ListUtxoRegular => {
                    let utxos = taker.get_wallet().list_descriptor_utxo_spend_info();
                    for utxo in utxos {
                        let utxo = UTXO::from_utxo_data(utxo);
                        println!("{}", serde_json::to_string_pretty(&utxo)?);
                    }
                }
                Commands::ListUtxoSwap => {
                    let utxos = taker.get_wallet().list_incoming_swap_coin_utxo_spend_info();
                    for utxo in utxos {
                        let utxo = UTXO::from_utxo_data(utxo);
                        println!("{}", serde_json::to_string_pretty(&utxo)?);
                    }
                }
                Commands::ListUtxoContract => {
                    let utxos = taker.get_wallet().list_live_timelock_contract_spend_info();
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
                    let address = taker
                        .get_wallet_mut()
                        .get_next_external_address(AddressType::P2WPKH)?;
                    println!("{address:?}");
                }
                Commands::SendToAddress {
                    address,
                    amount,
                    feerate,
                } => {
                    let amount = Amount::from_sat(*amount);

                    let manually_selected_outpoints = if cfg!(not(feature = "integration-test")) {
                        Some(
                            coinswap::utill::interactive_select(
                                taker.get_wallet().list_all_utxo_spend_info(),
                                amount,
                            )?
                            .iter()
                            .map(|(utxo, _)| bitcoin::OutPoint::new(utxo.txid, utxo.vout))
                            .collect::<Vec<_>>(),
                        )
                    } else {
                        None
                    };

                    let coins_to_spend = taker.get_wallet_mut().coin_select(
                        amount,
                        feerate.unwrap_or(MIN_FEE_RATE),
                        manually_selected_outpoints,
                    )?;

                    let outputs = vec![(Address::from_str(address)?.assume_checked(), amount)];
                    let destination = Destination::Multi {
                        outputs,
                        op_return_data: None,
                        change_address_type: AddressType::P2WPKH,
                    };

                    let tx = taker.get_wallet_mut().spend_from_wallet(
                        feerate.unwrap_or(MIN_FEE_RATE),
                        destination,
                        &coins_to_spend,
                    )?;

                    let txid = taker.get_wallet().send_tx(&tx).unwrap();

                    println!("{txid}");

                    taker.get_wallet_mut().sync_and_save()?;
                }
                Commands::FetchOffers => {
                    use std::time::{Duration, Instant};

                    println!("Waiting for offerbook synchronization to complete…");
                    let sync_start = Instant::now();

                    while taker.is_offerbook_syncing() {
                        println!("Offerbook sync in progress...");
                        std::thread::sleep(Duration::from_secs(2));
                    }

                    println!("Offerbook synchronized in {:.2?}", sync_start.elapsed());

                    let offerbook = taker.fetch_offers()?;
                    let makers = offerbook.all_makers();

                    if makers.is_empty() {
                        println!("No makers found in offerbook");
                        return Ok(());
                    }

                    let mut good = 0;
                    let mut bad = 0;
                    let mut unresponsive = 0;

                    println!("\nDiscovered {} makers\n", makers.len());

                    for maker in &makers {
                        match maker.state {
                            MakerState::Good => good += 1,
                            MakerState::Bad => bad += 1,
                            MakerState::Unresponsive { .. } => unresponsive += 1,
                        }

                        println!("{}", taker.display_offer(maker)?);
                    }

                    println!(
                        "\nOfferbook summary → good: {}, bad: {}, unresponsive: {} (total: {})",
                        good,
                        bad,
                        unresponsive,
                        makers.len()
                    );
                }
                Commands::Coinswap { makers, amount } => {
                    // Note: taproot coinswap is handled at the top level to avoid
                    // double Taker initialization. Regular ECDSA coinswap goes here.
                    let manually_selected_outpoints = if cfg!(not(feature = "integration-test")) {
                        let target_amount = Amount::from_sat(*amount);

                        Some(
                            coinswap::utill::interactive_select(
                                taker.get_wallet().list_all_utxo_spend_info(),
                                target_amount,
                            )?
                            .iter()
                            .map(|(utxo, _)| bitcoin::OutPoint::new(utxo.txid, utxo.vout))
                            .collect::<Vec<_>>(),
                        )
                    } else {
                        None
                    };

                    let swap_params = SwapParams {
                        send_amount: Amount::from_sat(*amount),
                        maker_count: *makers,
                        manually_selected_outpoints,
                    };
                    taker.do_coinswap(swap_params)?;
                }
                Commands::Recover => {
                    // Note: taproot recovery is handled at the top level to avoid
                    // overwriting the wallet file. Regular recovery goes here.
                    taker.recover_from_swap()?;
                }
                Commands::Backup { encrypt } => {
                    Wallet::backup_interactive(taker.get_wallet(), *encrypt);
                }
                _ => {}
            }
        }
    }

    Ok(())
}

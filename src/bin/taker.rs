use bitcoin::{Address, Amount};
use bitcoind::bitcoincore_rpc::Auth;
use clap::Parser;
use coinswap::{
    protocol::ProtocolVersion,
    taker::{
        error::TakerError, MakerState, UnifiedSwapParams, UnifiedTaker, UnifiedTakerConfig,
    },
    utill::{parse_proxy_auth, setup_taker_logger, MIN_FEE_RATE, UTXO},
    wallet::{AddressType, Destination, RPCConfig, Wallet},
};
use log::LevelFilter;
use serde_json::{json, to_string_pretty};
use std::{path::PathBuf, str::FromStr};

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
        /// Protocol version to use: "legacy" or "taproot"
        #[clap(long, default_value = "legacy")]
        protocol: String,
        /// Manually specify maker addresses (host:port). Can be repeated.
        /// When set, these makers are used directly instead of auto-discovery.
        #[clap(long = "maker-address")]
        maker_addresses: Vec<String>,
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

fn parse_protocol(s: &str) -> Result<ProtocolVersion, TakerError> {
    match s.to_lowercase().as_str() {
        "legacy" => Ok(ProtocolVersion::Legacy),
        "taproot" => Ok(ProtocolVersion::Taproot),
        _ => Err(TakerError::General(format!(
            "Unknown protocol '{}'. Use 'legacy' or 'taproot'.",
            s
        ))),
    }
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
        wallet_name: "random_1".to_string(), // updated during init
    };

    // Handle Restore before taker init (wallet may not exist yet)
    if let Commands::Restore { ref backup_file } = args.command {
        UnifiedTaker::restore_wallet(
            args.data_directory,
            args.wallet_name,
            Some(rpc_config),
            backup_file,
        );
        return Ok(());
    }

    // Build unified taker config
    let config = UnifiedTakerConfig {
        data_dir: args.data_directory,
        wallet_file_name: args.wallet_name,
        rpc_config: Some(rpc_config),
        tor_auth_password: args.tor_auth,
        zmq_addr: args.zmq,
        password: args.password,
        ..UnifiedTakerConfig::default()
    };

    let mut taker = UnifiedTaker::init(config)?;

    // Sync wallet after initialization
    taker.get_wallet().write().unwrap().sync_and_save()?;

    match &args.command {
        Commands::ListUtxo => {
            let wallet = taker.get_wallet().read().unwrap();
            let utxos = wallet.list_all_utxo_spend_info();
            for utxo in utxos {
                let utxo = UTXO::from_utxo_data(utxo);
                println!("{}", serde_json::to_string_pretty(&utxo)?);
            }
        }
        Commands::ListUtxoRegular => {
            let wallet = taker.get_wallet().read().unwrap();
            let utxos = wallet.list_descriptor_utxo_spend_info();
            for utxo in utxos {
                let utxo = UTXO::from_utxo_data(utxo);
                println!("{}", serde_json::to_string_pretty(&utxo)?);
            }
        }
        Commands::ListUtxoSwap => {
            let wallet = taker.get_wallet().read().unwrap();
            let utxos = wallet.list_incoming_swap_coin_utxo_spend_info();
            for utxo in utxos {
                let utxo = UTXO::from_utxo_data(utxo);
                println!("{}", serde_json::to_string_pretty(&utxo)?);
            }
        }
        Commands::ListUtxoContract => {
            let wallet = taker.get_wallet().read().unwrap();
            let utxos = wallet.list_live_timelock_contract_spend_info();
            for utxo in utxos {
                let utxo = UTXO::from_utxo_data(utxo);
                println!("{}", serde_json::to_string_pretty(&utxo)?);
            }
        }
        Commands::GetBalances => {
            let wallet = taker.get_wallet().read().unwrap();
            let balances = wallet.get_balances()?;
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
            let mut wallet = taker.get_wallet().write().unwrap();
            let address = wallet.get_next_external_address(AddressType::P2WPKH)?;
            println!("{address:?}");
        }
        Commands::SendToAddress {
            address,
            amount,
            feerate,
        } => {
            let amount = Amount::from_sat(*amount);

            let manually_selected_outpoints = if cfg!(not(feature = "integration-test")) {
                let wallet = taker.get_wallet().read().unwrap();
                Some(
                    coinswap::utill::interactive_select(
                        wallet.list_all_utxo_spend_info(),
                        amount,
                    )?
                    .iter()
                    .map(|(utxo, _)| bitcoin::OutPoint::new(utxo.txid, utxo.vout))
                    .collect::<Vec<_>>(),
                )
            } else {
                None
            };

            let mut wallet = taker.get_wallet().write().unwrap();
            let coins_to_spend =
                wallet.coin_select(amount, feerate.unwrap_or(MIN_FEE_RATE), manually_selected_outpoints)?;

            let outputs = vec![(Address::from_str(address)?.assume_checked(), amount)];
            let destination = Destination::Multi {
                outputs,
                op_return_data: None,
                change_address_type: AddressType::P2WPKH,
            };

            let tx = wallet.spend_from_wallet(
                feerate.unwrap_or(MIN_FEE_RATE),
                destination,
                &coins_to_spend,
            )?;

            let txid = wallet.send_tx(&tx).unwrap();
            println!("{txid}");

            wallet.sync_and_save()?;
        }
        Commands::FetchOffers => {
            use std::time::{Duration, Instant};

            println!("Waiting for offerbook synchronization to complete…");
            let sync_start = Instant::now();

            // Wait for the initial sync to finish
            while taker.is_offerbook_syncing() {
                std::thread::sleep(Duration::from_secs(1));
            }

            // If no makers found, the initial sync likely raced ahead of nostr
            // discovery. Wait briefly for nostr to discover makers, then re-sync.
            let offerbook = taker.fetch_offers()?;
            if offerbook.all_makers().is_empty() {
                println!("No makers found yet, waiting for network discovery…");
                std::thread::sleep(Duration::from_secs(5));
                taker.trigger_offerbook_sync();

                // Wait for the triggered sync to actually start (up to 3s)
                let wait_start = Instant::now();
                while !taker.is_offerbook_syncing()
                    && wait_start.elapsed() < Duration::from_secs(3)
                {
                    std::thread::sleep(Duration::from_millis(100));
                }

                // Now wait for the sync to finish
                while taker.is_offerbook_syncing() {
                    std::thread::sleep(Duration::from_secs(1));
                }
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
        Commands::Coinswap {
            makers,
            amount,
            protocol,
            maker_addresses,
        } => {
            let protocol_version = parse_protocol(protocol)?;

            let manually_selected_outpoints = if cfg!(not(feature = "integration-test")) {
                let target_amount = Amount::from_sat(*amount);
                let wallet = taker.get_wallet().read().unwrap();
                Some(
                    coinswap::utill::interactive_select(
                        wallet.list_all_utxo_spend_info(),
                        target_amount,
                    )?
                    .iter()
                    .map(|(utxo, _)| bitcoin::OutPoint::new(utxo.txid, utxo.vout))
                    .collect::<Vec<_>>(),
                )
            } else {
                None
            };

            let mut swap_params =
                UnifiedSwapParams::new(protocol_version, Amount::from_sat(*amount), *makers);
            swap_params.manually_selected_outpoints = manually_selected_outpoints;
            if !maker_addresses.is_empty() {
                swap_params.preferred_makers = Some(maker_addresses.clone());
            }

            taker.do_coinswap(swap_params)?;
        }
        Commands::Recover => {
            taker.recover_active_swap()?;
        }
        Commands::Backup { encrypt } => {
            let wallet = taker.get_wallet().read().unwrap();
            Wallet::backup_interactive(&wallet, *encrypt);
        }
        Commands::Restore { .. } => {
            // Handled above before taker init
            unreachable!()
        }
    }

    Ok(())
}

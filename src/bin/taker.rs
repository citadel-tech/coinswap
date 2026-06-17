use bitcoin::Amount;
use bitcoind::bitcoincore_rpc::Auth;
use clap::Parser;
use coinswap::{
    protocol::ProtocolVersion,
    taker::{
        error::TakerError, format_state, MakerOfferCandidate, MakerState, SwapParams, Taker,
        TakerInitConfig,
    },
    utill::{parse_proxy_auth, setup_taker_logger, UTXO},
    wallet::{AddressType, RPCConfig, Wallet},
};
use log::LevelFilter;
use serde_json::{json, to_string_pretty};
use std::{path::PathBuf, str::FromStr};

/// A simple command line app to operate as coinswap client.
///
/// The app works as a regular Bitcoin wallet with the added capability to perform coinswaps.
/// It can talk to either a Bitcoin Core node (over RPC + ZMQ — the default) or an
/// Electrum-protocol server (via `--electrum-url`). Both paths support the full swap flow
/// and the `restore` subcommand. It currently only runs on Testnet4.
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

    /// Bitcoin Core RPC address:port value. Ignored when `--electrum-url` is set.
    #[clap(
        name = "ADDRESS:PORT",
        long,
        short = 'r',
        default_value = "127.0.0.1:38332"
    )]
    pub rpc: String,

    /// Bitcoin Core ZMQ address:port value. Ignored when `--electrum-url` is set.
    #[clap(
        name = "ZMQ",
        long,
        short = 'z',
        default_value = "tcp://127.0.0.1:28332"
    )]
    pub zmq: String,

    /// Bitcoin Core RPC authentication string. Ex: username:password.
    /// Ignored when `--electrum-url` is set.
    #[clap(name="USER:PASSWORD",short='a',long, value_parser = parse_proxy_auth, default_value = "user:password")]
    pub auth: (String, String),
    #[clap(long, short = 't')]
    pub tor_auth: Option<String>,

    /// Electrum server URL (e.g. `tcp://localhost:50001`). When set, the wallet
    /// is initialised against an Electrum backend instead of Bitcoin Core.
    #[clap(name = "ELECTRUM_URL", long)]
    pub electrum_url: Option<String>,

    /// Sets the taker wallet's name. If the wallet file already exists, it will load that wallet. Default: taker-wallet
    #[clap(name = "WALLET", long, short = 'w')]
    pub wallet_name: Option<String>,

    /// Optional Password for the encryption of the wallet.
    #[clap(name = "PASSWORD", long, short = 'p')]
    pub password: Option<String>,

    /// Sets the verbosity level of debug.log file
    #[arg(long, short = 'v', value_parser = ["off", "error", "warn", "info", "debug", "trace"], default_value = "info")]
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

    /// List makers from the locally cached offerbook without triggering a network sync.
    ListOffers,
    /// Fetch an offer from a single maker address, verify the fidelity proof, and
    /// store the result in the offerbook. Adds the maker if absent.
    PollMaker {
        /// Maker onion address (e.g. `xyz.onion`).
        #[clap(long, short = 'm')]
        address: String,
    },
    /// Remove a maker from the local offerbook by address.
    RemoveMaker {
        /// Maker onion address (e.g. `xyz.onion`).
        #[clap(long, short = 'm')]
        address: String,
    },
    /// Initiate the coinswap process
    Coinswap {
        /// Sets the Maker count to swap with. Swapping with less than 2 makers is not allowed to maintain client privacy.
        /// Adding more makers in the swap will incur more swap fees.
        #[clap(long, short = 'm', default_value = "2")]
        makers: usize,
        /// Sets the swap amount in sats.
        #[clap(long, short = 'a', default_value = "20000")]
        amount: u64,
        #[clap(long = "tx-count", default_value = "1")]
        tx_count: u32,
        /// Protocol version to use: "legacy" or "taproot"
        #[clap(long, default_value = "legacy")]
        protocol: String,
        /// Manually specify maker addresses (host:port). Can be repeated.
        /// When set, these makers are used directly instead of auto-discovery.
        #[clap(long = "maker-address")]
        maker_addresses: Vec<String>,
        /// Automatically select UTXOs instead of interactive picker.
        #[clap(long)]
        auto_select: bool,
        /// Skip the confirmation prompt and proceed immediately.
        #[clap(long, short = 'y')]
        yes: bool,
        #[cfg(feature = "hotpath")]
        /// When enabled (and built with `--features 'hotpath hotpath-alloc'`), this will:
        /// - write a JSON report under `{data_dir}/hotpath/`
        /// - print timing + alloc tables after the swap completes
        #[clap(long)]
        hotpath: bool,
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
    /// Verify the deniability proof for a specific swap.
    VerifyDeniability {
        /// The swap ID to verify.
        #[clap(long, short = 's')]
        swap_id: String,
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

/// Display all makers with per-state counts and a summary line.
fn display_makers_with_summary<B: coinswap::wallet::BlockchainBackend>(
    wallet: &Wallet<B>,
    makers: &[MakerOfferCandidate],
) -> Result<(), TakerError> {
    let (mut good, mut bad, mut unresponsive) = (0, 0, 0);
    for maker in makers {
        match maker.state {
            MakerState::Good => good += 1,
            MakerState::Bad => bad += 1,
            MakerState::Unresponsive { .. } => unresponsive += 1,
        }
        println!("{}", display_offer(wallet, maker)?);
    }
    println!(
        "\nOfferbook summary → good: {}, bad: {}, unresponsive: {} (total: {})",
        good,
        bad,
        unresponsive,
        makers.len()
    );
    Ok(())
}

/// Format a maker offer candidate as a human-readable string.
fn display_offer<B: coinswap::wallet::BlockchainBackend>(
    wallet: &Wallet<B>,
    candidate: &MakerOfferCandidate,
) -> Result<String, TakerError> {
    let header = format!(
        r#"
    Maker
    ─────
    Address        : {address}
    Protocol       : {protocol}
    State          : {state}
    "#,
        address = candidate.address,
        protocol = candidate
            .protocol
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| "Unknown".into()),
        state = format_state(&candidate.state),
    );

    let Some(offer) = &candidate.offer else {
        return Ok(header);
    };

    let bond = &offer.fidelity.bond;
    let bond_value = wallet.calculate_bond_value(bond)?;

    Ok(format!(
        r#"{header}

    Offer
    ─────
    Base Fee       : {base_fee}
    Amount Fee %   : {amount_fee:.4}
    Time Fee %     : {time_fee:.4}

    Limits
    ──────
    Min Size       : {min_size}
    Max Size       : {max_size}
    Required Conf. : {confirms}
    Min Locktime   : {locktime}

    Fidelity Bond
    ─────────────
    Outpoint       : {outpoint}
    Value          : {bond_value}
    Expiry         : {expiry}
    "#,
        header = header.trim_end(),
        base_fee = offer.base_fee,
        amount_fee = offer.amount_relative_fee_pct,
        time_fee = offer.time_relative_fee_pct,
        min_size = offer.min_size,
        max_size = offer.max_size,
        confirms = offer.required_confirms,
        locktime = offer.minimum_locktime,
        outpoint = bond.outpoint(),
        bond_value = bond_value,
        expiry = bond.lock_time,
    ))
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

    let wallet_name = args
        .wallet_name
        .clone()
        .unwrap_or_else(|| "taker-wallet".to_string());
    let rpc_config = RPCConfig {
        url: args.rpc.clone(),
        auth: Auth::UserPass(args.auth.0.clone(), args.auth.1.clone()),
        wallet_name: wallet_name.clone(),
        zmq_addr: args.zmq.clone(),
    };

    // Build unified taker config (also used by the Restore branch).
    let backend = match args.electrum_url.as_ref() {
        Some(url) => coinswap::wallet::BackendConfig::Electrum(coinswap::wallet::ElectrumConfig {
            url: url.clone(),
            wallet_name,
        }),
        None => coinswap::wallet::BackendConfig::Bitcoind(rpc_config),
    };

    let config = TakerInitConfig {
        data_dir: args.data_directory.clone(),
        backend: backend.clone(),
        tor_auth_password: args.tor_auth.clone(),
        password: args.password.clone(),
        ..TakerInitConfig::default()
    };

    // `dispatch_backend` monomorphizes per backend, so the rest of the code (restore + init + run_commands) is written once over generic `B`.
    if args.electrum_url.is_some() {
        dispatch_backend::<coinswap::wallet::ElectrumBackend>(args, config, backend)
    } else {
        dispatch_backend::<coinswap::wallet::BitcoindBackend>(args, config, backend)
    }
}

fn dispatch_backend<B: coinswap::wallet::BlockchainBackend>(
    args: Cli,
    config: TakerInitConfig,
    backend: coinswap::wallet::BackendConfig,
) -> Result<(), TakerError> {
    if let Commands::Restore { ref backup_file } = args.command {
        coinswap::taker::Taker::<B>::restore_wallet(
            args.data_directory,
            args.wallet_name,
            backend,
            backup_file,
        );
        return Ok(());
    }
    let taker = Taker::<B>::init(config)?;
    run_commands(taker, &args)
}

fn run_commands<B: coinswap::wallet::BlockchainBackend>(
    mut taker: Taker<B>,
    args: &Cli,
) -> Result<(), TakerError> {
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
            let address = wallet.get_next_external_address(AddressType::P2TR)?;
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
                    coinswap::utill::interactive_select(wallet.list_all_utxo_spend_info(), amount)?
                        .iter()
                        .map(|(utxo, _)| bitcoin::OutPoint::new(utxo.txid, utxo.vout))
                        .collect::<Vec<_>>(),
                )
            } else {
                None
            };

            let mut wallet = taker.get_wallet().write().unwrap();
            let txid = wallet.send_to_address(
                amount.to_sat(),
                address.clone(),
                *feerate,
                manually_selected_outpoints,
            )?;
            println!("{txid}");
        }
        Commands::FetchOffers => {
            use std::time::Instant;

            println!("Waiting for offerbook synchronization to complete…");
            let sync_start = Instant::now();

            // Block until the offerbook sync cycle completes (includes Nostr discovery wait).
            taker.sync_offerbook_and_wait()?;

            println!("Offerbook synchronized in {:.2?}", sync_start.elapsed());

            let offerbook = taker.fetch_offers()?;
            let makers = offerbook.all_makers();

            if makers.is_empty() {
                println!("No makers found in offerbook");
                return Ok(());
            }

            println!("\nDiscovered {} makers\n", makers.len());

            let wallet = taker.get_wallet().read().unwrap();
            display_makers_with_summary(&wallet, &makers)?;
        }
        Commands::ListOffers => {
            let offerbook = taker.fetch_offers()?;
            let makers = offerbook.all_makers();

            if makers.is_empty() {
                println!(
                    "No makers in local offerbook. Run `fetch-offers` to sync from the network."
                );
                return Ok(());
            }

            println!("\n{} makers in local offerbook\n", makers.len());

            let wallet = taker.get_wallet().read().unwrap();
            display_makers_with_summary(&wallet, &makers)?;
        }
        Commands::PollMaker { address } => {
            let result = taker.poll_maker(address.clone())?;
            let wallet = taker.get_wallet().read().unwrap();
            println!("{}", display_offer(&wallet, &result)?);
        }
        Commands::RemoveMaker { address } => {
            let removed = taker.remove_maker(address.clone())?;
            if removed {
                println!("Removed maker {address} from offerbook");
            } else {
                println!("No maker with address {address} in offerbook");
            }
        }
        Commands::Coinswap {
            makers,
            amount,
            tx_count,
            protocol,
            maker_addresses,
            auto_select,
            yes,
            #[cfg(feature = "hotpath")]
            hotpath,
        } => {
            let protocol_version = parse_protocol(protocol)?;

            let manually_selected_outpoints =
                if !auto_select && cfg!(not(feature = "integration-test")) {
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
                SwapParams::new(protocol_version, Amount::from_sat(*amount), *makers);
            swap_params.tx_count = *tx_count;
            swap_params.manually_selected_outpoints = manually_selected_outpoints;
            if !maker_addresses.is_empty() {
                swap_params.preferred_makers = Some(maker_addresses.clone());
            }

            // Phase 1: Prepare — discover makers, negotiate, get fee summary.
            let summary = taker.prepare_coinswap(swap_params)?;

            println!("\n========== Swap Summary ==========");
            println!("Swap ID:   {}", summary.swap_id);
            println!("Protocol:  {:?}", summary.protocol);
            println!("Sending:   {}", summary.send_amount);
            println!();
            for (i, maker) in summary.makers.iter().enumerate() {
                println!("  Hop {}: {} ({:?})", i, maker.address, maker.protocol);
                println!(
                    "         Fees: base={} sats, amt={:.4}%, time={:.6}%",
                    maker.base_fee, maker.amount_relative_fee_pct, maker.time_relative_fee_pct
                );
                println!(
                    "         Locktime: {} blocks, Estimated fee: {} sats",
                    maker.locktime, maker.estimated_fee_sats
                );
            }
            println!();
            println!("Total estimated fee: {}", summary.total_estimated_fee);
            println!("Estimated receive:   {}", summary.estimated_receive_amount);
            println!("==================================\n");

            // In integration tests, skip the confirmation prompt.
            if !yes && cfg!(not(feature = "integration-test")) {
                print!("Proceed with this swap? [y/N] ");
                use std::io::{self, Write};
                io::stdout().flush().unwrap();
                let mut input = String::new();
                io::stdin()
                    .read_line(&mut input)
                    .map_err(|e| TakerError::General(format!("Failed to read input: {:?}", e)))?;
                let input = input.trim().to_lowercase();
                if input != "y" && input != "yes" {
                    println!("Swap cancelled.");
                    return Ok(());
                }
            }

            // Phase 2: Execute — commit funds and complete the swap.
            #[cfg(feature = "hotpath")]
            let hotpath_run = if *hotpath {
                let data_dir = args
                    .data_directory
                    .clone()
                    .unwrap_or_else(coinswap::utill::get_taker_dir);

                Some(
                    coinswap::hotpath_local::HotpathRun::start(
                        "coinswap_taker_swap",
                        coinswap::hotpath_local::default_report_path(
                            &data_dir,
                            "taker_swap",
                            &summary.swap_id,
                        ),
                    )
                    .map_err(|e| TakerError::General(format!("Failed to start Hotpath: {e:?}")))?,
                )
            } else {
                None
            };

            taker.start_coinswap(&summary.swap_id)?;

            #[cfg(feature = "hotpath")]
            if let Some(run) = hotpath_run {
                run.finish_and_print();
            }
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
        Commands::VerifyDeniability { swap_id } => match taker.verify_deniability(swap_id) {
            Ok(true) => println!("Proof valid: swap participated in a completed coinswap"),
            Ok(false) => println!("Proof invalid or not found for this swap ID"),
            Err(e) => println!("Error: {e}"),
        },
    }

    Ok(())
}

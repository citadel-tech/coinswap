//! The Coinswap Maker Server.
//!
//! This module includes all server side code for the coinswap maker.
//! The server maintains the thread pool for P2P Connection, Watchtower, Bitcoin Backend, and RPC Client Request.
//! The server listens at two ports: 6102 for P2P, and 6103 for RPC Client requests.

use crate::{
    protocol::messages::FidelityProof,
    utill::{COINSWAP_KIND, NOSTR_RELAYS},
};
use bitcoin::{absolute::LockTime, Amount};
use bitcoind::bitcoincore_rpc::RpcApi;
use nostr::{
    event::{EventBuilder, Kind},
    key::{Keys, SecretKey},
    message::{ClientMessage, RelayMessage},
    util::JsonUtil,
};
use tungstenite::Message;

use std::{
    io::ErrorKind,
    net::{Ipv4Addr, TcpListener, TcpStream},
    sync::{
        atomic::Ordering::{self, Relaxed},
        Arc,
    },
    thread::{self, sleep},
    time::Duration,
};

use crate::utill::get_tor_hostname;

pub(crate) use super::{api::RPC_PING_INTERVAL, Maker};

use crate::{
    error::NetError,
    maker::{
        api::{
            check_for_broadcasted_contracts, check_for_idle_states,
            restore_broadcasted_contracts_on_reboot, ConnectionState,
            FIDELITY_BOND_UPDATE_INTERVAL, SWAP_LIQUIDITY_CHECK_INTERVAL,
        },
        handlers::handle_message,
        rpc::start_rpc_server,
    },
    protocol::messages::TakerToMakerMessage,
    utill::{read_message, send_message, HEART_BEAT_INTERVAL, MIN_FEE_RATE},
    wallet::{AddressType, WalletError},
};

use crate::maker::error::MakerError;

/// Fetches the Maker
/// Depending upon ConnectionType and test/prod environment, different maker address are returned.
/// Return the Maker address
fn network_bootstrap(maker: Arc<Maker>) -> Result<String, MakerError> {
    let maker_port = maker.config.network_port;
    let maker_address = {
        if cfg!(feature = "integration-test") {
            // Always clearnet in integration tests
            format!("127.0.0.1:{maker_port}")
        } else {
            // Always Tor otherwise
            let maker_hostname = get_tor_hostname(
                maker.get_data_dir(),
                maker.config.control_port,
                maker_port,
                &maker.config.tor_auth_password,
            )?;
            format!("{maker_hostname}:{maker_port}")
        }
    };

    maker
        .as_ref()
        .track_and_update_unconfirmed_fidelity_bonds()?;

    manage_fidelity_bonds(maker, &maker_address, true)?;

    Ok(maker_address)
}

/// Manages the maker's fidelity bonds and ensures the server is updated with the latest bond proof and maker address.
///
/// It performs the following operations:
/// 1. Redeems all expired fidelity bonds in the maker's wallet, if any are found.
/// 2. Creates a new fidelity bond if no valid bonds remain after redemption.
fn manage_fidelity_bonds(
    maker: Arc<Maker>,
    maker_addr: &str,
    spawn_nostr: bool,
) -> Result<(), MakerError> {
    maker
        .wallet
        .write()?
        .redeem_expired_fidelity_bonds(AddressType::P2WPKH)?;

    let fidelity = setup_fidelity_bond(maker.as_ref(), maker_addr)?;

    if spawn_nostr {
        spawn_nostr_broadcast_task(fidelity, maker)?;
    }

    Ok(())
}
fn spawn_nostr_broadcast_task(
    fidelity: FidelityProof,
    maker: Arc<Maker>,
) -> Result<(), MakerError> {
    log::info!("Spawning nostr background task for maker");
    let maker_clone = maker.clone();

    let handle = thread::Builder::new()
        .name("nostr-event-thread".to_string())
        .spawn(move || {
            if let Err(e) = broadcast_bond_on_nostr(fidelity.clone()) {
                log::warn!("initial nostr broadcast failed: {:?}", e);
            }

            let interval = Duration::from_secs(30 * 60);
            let tick = Duration::from_secs(2);
            let mut elapsed = Duration::ZERO;

            while !maker_clone.shutdown.load(Ordering::Acquire) {
                thread::sleep(tick);
                elapsed += tick;

                if elapsed < interval {
                    continue;
                }

                elapsed = Duration::ZERO;

                log::debug!("re-pinging nostr relays with bond announcement");

                if let Err(e) = broadcast_bond_on_nostr(fidelity.clone()) {
                    log::warn!("nostr re-ping failed: {:?}", e);
                }
            }

            log::info!("nostr background task stopped");
        })?;

    maker.thread_pool.add_thread(handle);
    Ok(())
}

// ##TODO: Make this part of nostr module and improve error handing
// ##TODO: Try retry in case relay doesn't accept the event
fn broadcast_bond_on_nostr(fidelity: FidelityProof) -> Result<(), MakerError> {
    let outpoint = fidelity.bond.outpoint;
    let content = format!("{}:{}", outpoint.txid, outpoint.vout);

    // ##TODO: Don't use ephemeral keys
    let secret_key = SecretKey::generate();
    let keys = Keys::new(secret_key);

    let event = EventBuilder::new(Kind::Custom(COINSWAP_KIND), content)
        .build(keys.public_key)
        .sign_with_keys(&keys)
        .expect("Event should be signed");

    let msg = ClientMessage::Event(std::borrow::Cow::Owned(event));

    log::debug!("nostr wire msg: {}", msg.as_json());

    let mut success = false;

    for relay in NOSTR_RELAYS {
        match broadcast_to_relay(relay, &msg) {
            Ok(()) => {
                success = true;
            }
            Err(e) => {
                log::warn!("failed to broadcast to {}: {:?}", relay, e);
            }
        }
    }

    if !success {
        log::warn!("nostr event was not accepted by any relay");
    }

    Ok(())
}

fn broadcast_to_relay(relay: &str, msg: &ClientMessage) -> Result<(), MakerError> {
    let (mut socket, _) = tungstenite::connect(relay).map_err(|e| {
        log::warn!("failed to connect to nostr relay {}: {}", relay, e);
        MakerError::General("failed to connect to nostr relay")
    })?;

    socket
        .write(Message::Text(msg.as_json().into()))
        .map_err(|e| {
            log::warn!("nostr relay write failed: {}", e);
            MakerError::General("failed to write to nostr relay")
        })?;
    socket.flush().ok();

    match socket.read() {
        Ok(Message::Text(text)) => {
            if let Ok(relay_msg) = RelayMessage::from_json(&text) {
                match relay_msg {
                    RelayMessage::Ok {
                        event_id,
                        status: true,
                        ..
                    } => {
                        log::info!("nostr relay {} accepted event {}", relay, event_id);
                        return Ok(());
                    }
                    RelayMessage::Ok {
                        event_id,
                        status: false,
                        message,
                    } => {
                        log::warn!(
                            "nostr relay {} rejected event {}: {}",
                            relay,
                            event_id,
                            message
                        );
                    }
                    _ => {}
                }
            }
        }
        Ok(_) => {}
        Err(e) => {
            log::warn!("nostr relay {} read error: {}", relay, e);
        }
    }
    log::warn!("nostr relay {} did not confirm event", relay);

    Err(MakerError::General("nostr relay did not confirm event"))
}

/// Ensures the wallet has a valid fidelity bond. If no active bond exists, it creates a new one.
///
/// ### NOTE ON VALID FIDELITY BOND:
/// A valid fidelity bond is one that has not expired, been redeemed, or spent.
///
/// ## Returns:
/// - The highest **FidelityProof**, proving ownership of the highest valid fidelity bond, the maker has.
fn setup_fidelity_bond(maker: &Maker, maker_address: &str) -> Result<FidelityProof, MakerError> {
    let highest_index = maker.get_wallet().read()?.get_highest_fidelity_index()?;
    let mut proof = maker.highest_fidelity_proof.write()?;

    if let Some(i) = highest_index {
        let wallet_read = maker.get_wallet().read()?;
        let bond = wallet_read.store.fidelity_bond.get(&i).unwrap();

        let current_height = wallet_read
            .rpc
            .get_block_count()
            .map_err(WalletError::Rpc)? as u32;

        let highest_proof = maker
            .get_wallet()
            .read()?
            .generate_fidelity_proof(i, maker_address)?;

        log::info!(
            "Highest bond at outpoint {} | index {} | Amount {:?} sats | Remaining Timelock for expiry : {:?} Blocks | Current Bond Value : {:?} sats",
            highest_proof.bond.outpoint,
            i,
            bond.amount.to_sat(),
            bond.lock_time.to_consensus_u32() - current_height,
            wallet_read.calculate_bond_value(bond)?.to_sat()
        );

        *proof = Some(highest_proof);
    } else {
        log::info!("No active Fidelity Bonds found. Creating one.");

        let amount = Amount::from_sat(maker.config.fidelity_amount);

        log::info!("Fidelity value chosen = {:?} sats", amount.to_sat());

        let current_height = maker
            .get_wallet()
            .read()?
            .rpc
            .get_block_count()
            .map_err(WalletError::Rpc)? as u32;

        // Set 950 blocks locktime for test
        let locktime = if cfg!(feature = "integration-test") {
            LockTime::from_height(current_height + 950).map_err(WalletError::Locktime)?
        } else {
            LockTime::from_height(maker.config.fidelity_timelock + current_height)
                .map_err(WalletError::Locktime)?
        };

        log::info!(
            "Fidelity timelock {:?} blocks",
            locktime.to_consensus_u32() - current_height
        );

        let sleep_increment = 10;
        let mut sleep_multiplier = 0;

        while !maker.shutdown.load(Relaxed) {
            sleep_multiplier += 1;
            // sync the wallet
            log::info!("Sync at:----setup_fidelity_bond----");
            maker.get_wallet().write()?.sync_and_save()?;

            let fidelity_result = maker.get_wallet().write()?.create_fidelity(
                amount,
                locktime,
                Some(maker_address),
                MIN_FEE_RATE,
                AddressType::P2WPKH,
            );

            match fidelity_result {
                // Wait for sufficient funds to create fidelity bond.
                // Hard error if fidelity still can't be created.
                Err(e) => {
                    if let WalletError::InsufficientFund {
                        available,
                        required,
                    } = e
                    {
                        log::warn!("Insufficient fund to create fidelity bond.");
                        let amount = required - available;
                        let addr = maker
                            .get_wallet()
                            .write()?
                            .get_next_external_address(AddressType::P2WPKH)?;

                        log::info!("Send at least {:.8} BTC to {:?} | If you send extra, that will be added to your wallet balance", Amount::from_sat(amount).to_btc(), addr);

                        let total_sleep = sleep_increment * sleep_multiplier.min(10 * 60);
                        log::info!("Next sync in {total_sleep:?} secs");
                        thread::sleep(Duration::from_secs(total_sleep));
                    } else {
                        log::error!(
                            "[{}] Fidelity Bond Creation failed: {:?}. Shutting Down Maker server",
                            maker.config.network_port,
                            e
                        );
                        return Err(e.into());
                    }
                }
                Ok(i) => {
                    log::info!(
                        "[{}] Successfully created fidelity bond",
                        maker.config.network_port
                    );
                    let highest_proof = maker
                        .get_wallet()
                        .read()?
                        .generate_fidelity_proof(i, maker_address)?;

                    *proof = Some(highest_proof);

                    // sync and save the wallet data to disk
                    log::info!(" Sync at end:----setup_fidelity_bond----");
                    maker.get_wallet().write()?.sync_and_save()?;
                    break;
                }
            }
        }
    };

    Ok(proof
        .clone()
        .expect("Fidelity Proof must exist after creating a bond"))
}

/// Checks if the maker has enough liquidity for swaps.
/// If funds are below the minimum required, it repeatedly prompts the user to add more
/// until the liquidity is sufficient.
fn check_swap_liquidity(maker: &Maker) -> Result<(), MakerError> {
    let sleep_incremental = 10;
    let mut sleep_duration = 0;
    let addr = maker
        .get_wallet()
        .write()?
        .get_next_external_address(AddressType::P2WPKH)?;
    while !maker.shutdown.load(Relaxed) {
        log::info!("Sync at:----check_swap_liquidity----");
        maker.get_wallet().write()?.sync_and_save()?;
        let offer_max_size = maker.get_wallet().read()?.store.offer_maxsize;

        let min_required = maker.config.min_swap_amount;
        if offer_max_size < min_required {
            log::warn!(
                "Low Swap Liquidity | Min: {min_required} sats | Available: {offer_max_size} sats. Add funds to {addr:?}"
            );

            sleep_duration = (sleep_duration + sleep_incremental).min(10 * 60); // Capped at 1 Block interval
            log::info!("Next sync in {sleep_duration:?} secs");
            thread::sleep(Duration::from_secs(sleep_duration));
        } else {
            log::info!(
                "Swap Liquidity: {offer_max_size} sats | Min: {min_required} sats | Listening for requests."
            );
            break;
        }
    }

    Ok(())
}

/// Continuously checks if the Bitcoin Core RPC connection is live.
fn check_connection_with_core(maker: &Maker) -> Result<(), MakerError> {
    let mut rcp_ping_success = true;
    while !maker.shutdown.load(Relaxed) {
        if let Err(e) = maker.wallet.read()?.rpc.get_blockchain_info() {
            log::error!(
                "[{}] RPC Connection failed | Error: {} | Reattempting...",
                maker.config.network_port,
                e
            );
            rcp_ping_success = false;
        } else {
            if !rcp_ping_success {
                log::info!(
                    "[{}] Bitcoin Core RPC connection is live.",
                    maker.config.network_port
                );
            }

            break;
        }

        thread::sleep(HEART_BEAT_INTERVAL);
    }

    Ok(())
}

/// Handle a single client connection.
fn handle_client(maker: &Arc<Maker>, stream: &mut TcpStream) -> Result<(), MakerError> {
    stream.set_nonblocking(false)?; // Block this thread until message is read.

    let mut connection_state = ConnectionState::default();

    while !maker.shutdown.load(Relaxed) {
        let mut bytes = Vec::new();
        match read_message(stream) {
            Ok(b) => bytes = b,
            Err(e) => {
                if let NetError::IO(e) = e {
                    if e.kind() == ErrorKind::UnexpectedEof {
                        log::info!("[{}] Connection ended.", maker.config.network_port);
                        break;
                    } else {
                        // For any other errors, report them
                        log::error!("[{}] Net Error: {}", maker.config.network_port, e);
                        continue;
                    }
                }
            }
        }
        let taker_msg = serde_cbor::from_slice::<TakerToMakerMessage>(&bytes)?;

        log::info!("[{}] <=== {}", maker.config.network_port, taker_msg);

        let reply = handle_message(maker, &mut connection_state, taker_msg);

        match reply {
            Ok(reply) => {
                if let Some(message) = reply {
                    log::info!("[{}] ===> {} ", maker.config.network_port, message);
                    if let Err(e) = send_message(stream, &message) {
                        log::error!("Closing due to IO error in sending message: {e:?}");
                        continue;
                    }
                } else {
                    continue;
                }
            }
            Err(err) => {
                match &err {
                    MakerError::SpecialBehaviour(sp) => {
                        log::error!(
                            "[{}] Maker Special Behavior Triggered Disconnection : {:?}",
                            maker.config.network_port,
                            sp
                        );
                    }
                    e => {
                        log::error!(
                            "[{}] Internal message handling error occurred: {:?}",
                            maker.config.network_port,
                            e
                        );
                    }
                }
                break;
            }
        }
    }

    Ok(())
}

/// Starts the Maker server and manages its core operations.
///
/// This function initializes network connections, sets up the wallet with fidelity bonds,  
/// and spawns essential threads for:  
/// - Checking for idle client connections.  
/// - Detecting and handling broadcasted contract transactions.  
/// - Running an RPC server for communication with `maker-cli`.  
///
/// The server continuously listens for incoming P2P client connections.
/// It performs periodic checks to ensure liquidity availability, update fidelity bonds,  
/// and maintain backend connectivity while avoiding interruptions during active swaps.  
///
/// The server continues to run until a shutdown signal is detected, at which point
/// it performs cleanup tasks, such as sync and saving wallet data, joining all threads, etc.
pub fn start_maker_server(maker: Arc<Maker>) -> Result<(), MakerError> {
    log::info!("Starting Maker Server");

    // Setup the wallet with fidelity bond.
    let maker_addr = network_bootstrap(maker.clone())?;

    // Tracks the elapsed time in heartbeat intervals to schedule periodic checks and avoid redundant executions.
    let mut interval_tracker = 0;

    check_swap_liquidity(maker.as_ref())?;

    // HEART_BEAT_INTERVAL secs are added to prevent redundant checks for swap liquidity immediately after the Maker server starts.
    // This ensures these functions are not executed twice in quick succession.
    interval_tracker += HEART_BEAT_INTERVAL.as_secs() as u32;

    let network_port = maker.config.network_port;

    {
        let wallet = maker.get_wallet().read()?;
        log::info!(
            "[{}] Bitcoin Network: {}",
            network_port,
            wallet.store.network
        );
        log::info!(
            "[{}] Spendable Wallet Balance: {}",
            network_port,
            wallet.get_balances()?.spendable
        );
    }

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, maker.config.network_port))
        .map_err(NetError::IO)?;
    listener.set_nonblocking(true)?; // Needed to not block a thread waiting for incoming connection.

    if !maker.shutdown.load(Relaxed) {
        // 1. Idle Client connection checker thread.
        // This threads check idleness of peer in live swaps.
        // And takes recovery measures if the peer seems to have disappeared in middle of a swap.
        let maker_clone = maker.clone();
        let idle_conn_check_thread = thread::Builder::new()
            .name("Idle Client Checker Thread".to_string())
            .spawn(move || {
                log::info!("[{network_port}] Spawning Client connection status checker thread");
                if let Err(e) = check_for_idle_states(maker_clone.clone()) {
                    log::error!("Failed checking client's idle state {e:?}");
                    maker_clone.shutdown.store(true, Relaxed);
                }
            })?;
        maker.thread_pool.add_thread(idle_conn_check_thread);

        // 2. Watchtower thread.
        // This thread checks for broadcasted contract transactions, which usually means a violation of the protocol.
        // When a contract transaction is detected in mempool it will attempt recovery.
        // This can get triggered even when contracts of adjacent hops are published. Implying the whole swap route is disrupted.
        let maker_clone = maker.clone();
        let contract_watcher_thread = thread::Builder::new()
            .name("Contract Watcher Thread".to_string())
            .spawn(move || {
                log::info!("[{network_port}] Spawning contract-watcher thread");
                if let Err(e) = check_for_broadcasted_contracts(maker_clone.clone()) {
                    maker_clone.shutdown.store(true, Relaxed);
                    log::error!("Failed checking broadcasted contracts {e:?}");
                }
            })?;
        maker.thread_pool.add_thread(contract_watcher_thread);

        // 3: The RPC server thread.
        // User for responding back to `maker-cli` apps.
        let maker_clone = maker.clone();
        let rpc_thread = thread::Builder::new()
            .name("RPC Thread".to_string())
            .spawn(move || {
                log::info!("[{network_port}] Spawning RPC server thread");
                match start_rpc_server(maker_clone.clone()) {
                    Ok(_) => (),
                    Err(e) => {
                        log::error!("Failed starting rpc server {e:?}");
                        maker_clone.shutdown.store(true, Relaxed);
                    }
                }
            })?;

        maker.thread_pool.add_thread(rpc_thread);

        sleep(HEART_BEAT_INTERVAL); // wait for 1 beat, to complete spawns of all the threads.

        // Check if recovery is needed.
        let (inc, out) = maker.wallet.read()?.find_unfinished_swapcoins();
        if !inc.is_empty() || !out.is_empty() {
            log::info!("Incomplete swaps detected in the wallet. Starting recovery");
            restore_broadcasted_contracts_on_reboot(&maker)?;
        }

        maker.is_setup_complete.store(true, Relaxed);
        log::info!("[{}] Server Setup completed!! Use maker-cli to operate the server and the internal wallet.", maker.config.network_port);
    }

    while !maker.shutdown.load(Relaxed) {
        if interval_tracker.is_multiple_of(RPC_PING_INTERVAL) {
            check_connection_with_core(maker.as_ref())?;
        }

        // Perform fidelity bond and liquidity checks only when no coinswap is in progress.
        // This prevents the server from getting blocked while creating a new bond or waiting
        // for additional funds, which could otherwise interrupt an ongoing swap.
        // Running these checks during an active swap might cause the maker to stop responding,
        // potentially aborting the swap.
        if maker.ongoing_swap_state.lock()?.is_empty() {
            if interval_tracker.is_multiple_of(FIDELITY_BOND_UPDATE_INTERVAL) {
                manage_fidelity_bonds(maker.clone(), &maker_addr, false)?;
                interval_tracker = 0;
            }

            if interval_tracker.is_multiple_of(FIDELITY_BOND_UPDATE_INTERVAL) {
                check_swap_liquidity(maker.as_ref())?;
            }
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                log::info!("[{network_port}] Received incoming connection");

                if let Err(e) = handle_client(&maker, &mut stream) {
                    log::error!("[{network_port}] Error Handling client request {e:?}");
                }
            }

            Err(e) => {
                if e.kind() != ErrorKind::WouldBlock {
                    log::error!("[{network_port}] Error accepting incoming connection: {e:?}");
                }
            }
        };

        // Increment **interval_tracker** only if no coinswap is in progress or if no pending
        // swap liquidity and fidelity bond checks are due. This ensures these checks are
        // not skipped due to an ongoing coinswap and are performed once it completes.
        if maker.ongoing_swap_state.lock()?.is_empty()
            || !interval_tracker.is_multiple_of(SWAP_LIQUIDITY_CHECK_INTERVAL)
            || !interval_tracker.is_multiple_of(FIDELITY_BOND_UPDATE_INTERVAL)
        {
            interval_tracker += HEART_BEAT_INTERVAL.as_secs() as u32;
        }

        sleep(HEART_BEAT_INTERVAL);
    }

    log::info!("[{network_port}] Maker is shutting down.");
    maker.thread_pool.join_all_threads()?;

    log::info!("sync at:----Shutdown wallet----");
    maker.get_wallet().write()?.sync_and_save()?;
    log::info!("Maker Server is shut down successfully.");
    Ok(())
}

//! The Coinswap Maker Server for Taproot protocol.
//!
//! This module includes all server side code for the coinswap maker using the new taproot protocol.
//! The server maintains the thread pool for P2P Connection, Watchtower, Bitcoin Backend, and RPC Client Request.
//! It uses the new message protocol (messages2) and integrates with api2.rs and handlers2.rs.
//! The server listens at two ports: 6102 for P2P, and 6103 for RPC Client requests.

use bitcoin::Amount;
use bitcoind::bitcoincore_rpc::RpcApi;
use std::{
    io::ErrorKind,
    net::{Ipv4Addr, TcpListener, TcpStream},
    sync::{
        atomic::Ordering::{self, Relaxed},
        Arc,
    },
    thread::{self, sleep},
    time::{Duration, Instant},
};

pub(crate) use super::api2::{Maker, RPC_PING_INTERVAL};

use crate::{
    error::NetError,
    maker::{
        api::FIDELITY_BOND_UPDATE_INTERVAL,
        api2::{check_for_broadcasted_contracts, check_for_idle_states, ConnectionState},
        handlers2::handle_message_taproot,
        rpc::start_rpc_server,
    },
    nostr_coinswap::broadcast_bond_on_nostr,
    protocol::{
        messages2::{MakerToTakerMessage, TakerToMakerMessage},
    },
    utill::{get_tor_hostname, read_message, send_message, HEART_BEAT_INTERVAL, MIN_FEE_RATE},
    wallet::{AddressType, WalletError},
};

use crate::maker::error::MakerError;
#[cfg(feature = "integration-test")]
use crate::maker::TaprootMakerBehavior;

/// Fetches the Maker address.
fn network_bootstrap_taproot(maker: Arc<Maker>) -> Result<String, MakerError> {
    let maker_port = maker.config.network_port;
    let maker_address = if cfg!(feature = "integration-test") {
        // Always clearnet in integration tests
        let maker_address = format!("127.0.0.1:{maker_port}");
        maker_address
    } else {
        // Always Tor in production
        let maker_hostname = get_tor_hostname(
            maker.data_dir(),
            maker.config.control_port,
            maker_port,
            &maker.config.tor_auth_password,
        )?;
        let maker_address = format!("{maker_hostname}:{maker_port}");
        maker_address
    };

    // Track and update unconfirmed fidelity bonds
    maker
        .as_ref()
        .track_and_update_unconfirmed_fidelity_bonds()?;

    manage_fidelity_bonds_taproot(maker, &maker_address, true)?;

    Ok(maker_address)
}

/// Setup's maker fidelity
fn manage_fidelity_bonds_taproot(
    maker: Arc<Maker>,
    maker_addr: &str,
    spawn_nostr: bool,
) -> Result<(), MakerError> {
    // Redeem expired fidelity bonds first
    maker
        .wallet()
        .write()?
        .redeem_expired_fidelity_bonds(AddressType::P2TR)?;

    setup_fidelity_bond_taproot(maker.as_ref(), maker_addr)?;

    if spawn_nostr {
        spawn_nostr_broadcast_task(maker)?;
    }

    Ok(())
}

fn spawn_nostr_broadcast_task(maker: Arc<Maker>) -> Result<(), MakerError> {
    log::info!("Spawning nostr background task for maker");
    let maker_clone = maker.clone();

    let handle = thread::Builder::new()
        .name("nostr-event-thread".to_string())
        .spawn(move || {
            let interval = Duration::from_secs(30 * 60);
            let tick = Duration::from_secs(2);
            // Force a first broadcast right after spawn.
            let mut elapsed = interval;
            let mut last_broadcasted_outpoint = None;
            let mut last_attempted_outpoint = None;

            while !maker_clone.shutdown.load(Ordering::Acquire) {
                let latest_fidelity = match maker_clone.highest_fidelity_proof.read() {
                    Ok(proof) => proof.clone(),
                    Err(e) => {
                        log::warn!("failed to read latest fidelity proof for nostr broadcast: {e}");
                        None
                    }
                };

                if let Some(fidelity) = latest_fidelity {
                    let latest_outpoint = fidelity.bond.outpoint;
                    let outpoint_changed = last_attempted_outpoint
                        .map(|prev| prev != latest_outpoint)
                        .unwrap_or(true);
                    let periodic_reping_due = elapsed >= interval;

                    if outpoint_changed || periodic_reping_due {
                        if outpoint_changed && last_broadcasted_outpoint.is_some() {
                            log::info!(
                                "Detected updated fidelity outpoint {}; broadcasting updated nostr announcement",
                                latest_outpoint
                            );
                        } else if outpoint_changed {
                            log::info!("Broadcasting initial fidelity bond announcement");
                        } else if periodic_reping_due {
                            log::debug!("re-pinging nostr relays with bond announcement");
                        }

                        last_attempted_outpoint = Some(latest_outpoint);
                        match broadcast_bond_on_nostr(fidelity, maker_clone.config.socks_port) {
                            Ok(()) => {
                                last_broadcasted_outpoint = Some(latest_outpoint);
                            }
                            Err(e) => {
                                log::warn!("nostr bond broadcast failed: {:?}", e);
                            }
                        }

                        elapsed = Duration::ZERO;
                    }
                }

                thread::sleep(tick);
                elapsed += tick;
            }

            log::info!("nostr background task stopped");
        })?;

    maker.thread_pool.add_thread(handle);
    Ok(())
}

/// Ensures the wallet has a valid fidelity bond for taproot operations.
/// This follows the same pattern as the regular maker setup_fidelity_bond function.
fn setup_fidelity_bond_taproot(
    maker: &Maker,
    maker_address: &str,
) -> Result<crate::protocol::messages::FidelityProof, MakerError> {
    use crate::wallet::WalletError;
    use bitcoin::absolute::LockTime;
    use std::thread;

    let highest_index = maker.wallet().read()?.get_highest_fidelity_index()?;

    if let Some(i) = highest_index {
        let wallet_read = maker.wallet().read()?;
        let bond = wallet_read.store.fidelity_bond.get(&i).unwrap().clone();
        let current_height = wallet_read
            .rpc
            .get_block_count()
            .map_err(WalletError::Rpc)? as u32;
        let bond_value = wallet_read.calculate_bond_value(&bond)?.to_sat();
        drop(wallet_read);

        let proof_message = maker
            .wallet()
            .read()?
            .generate_fidelity_proof(i, maker_address)?;

        let highest_proof = crate::protocol::messages::FidelityProof {
            bond: proof_message.bond.clone(),
            cert_hash: *bitcoin::hashes::sha256d::Hash::from_bytes_ref(
                proof_message.cert_hash.as_ref(),
            ),
            cert_sig: proof_message.cert_sig,
        };

        log::info!(
            "[{}] Using existing fidelity bond at outpoint {} | index {} | Amount {:?} sats | Remaining Timelock for expiry : {:?} Blocks | Current Bond Value : {:?} sats",
            maker.config.network_port,
            proof_message.bond.outpoint,
            i,
            bond.amount.to_sat(),
            bond.lock_time.to_consensus_u32() - current_height,
            bond_value
        );

        // Store the fidelity proof in maker
        *maker.highest_fidelity_proof.write()? = Some(highest_proof.clone());

        return Ok(highest_proof);
    }

    // No active Fidelity Bonds found. Creating one.
    log::info!(
        "[{}] No active Fidelity Bonds found. Creating one.",
        maker.config.network_port
    );

    let amount = Amount::from_sat(maker.config.fidelity_amount);

    log::info!(
        "[{}] Fidelity value chosen = {:?} sats",
        maker.config.network_port,
        amount.to_sat()
    );

    let current_height = maker
        .wallet()
        .read()?
        .rpc
        .get_block_count()
        .map_err(WalletError::Rpc)? as u32;

    // Set locktime for fidelity bond
    #[cfg(feature = "integration-test")]
    let locktime = {
        if maker.behavior == TaprootMakerBehavior::InvalidFidelityTimelock {
            LockTime::from_height(current_height + 500).map_err(WalletError::Locktime)?
        } else {
            LockTime::from_height(current_height + 950).map_err(WalletError::Locktime)?
        }
    };
    #[cfg(not(feature = "integration-test"))]
    let locktime = LockTime::from_height(maker.config.fidelity_timelock + current_height)
        .map_err(WalletError::Locktime)?;

    log::info!(
        "[{}] Fidelity timelock {:?} blocks",
        maker.config.network_port,
        locktime.to_consensus_u32() - current_height
    );

    let sleep_increment = 10;
    let mut sleep_multiplier = 0;

    while !maker.shutdown.load(Relaxed) {
        sleep_multiplier += 1;
        // sync the wallet
        log::info!("Sync at:----setup_fidelity_bond----");
        maker.wallet().write()?.sync_and_save()?;

        let fidelity_result = maker.wallet().write()?.create_fidelity(
            amount,
            locktime,
            Some(maker_address),
            MIN_FEE_RATE,
            AddressType::P2TR,
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
                    log::warn!(
                        "[{}] Insufficient fund to create fidelity bond.",
                        maker.config.network_port
                    );
                    let amount = required - available;
                    let addr = maker
                        .wallet()
                        .write()?
                        .get_next_external_address(AddressType::P2TR)?;

                    log::info!("[{}] Send at least {:.8} BTC to {:?} | If you send extra, that will be added to your wallet balance", maker.config.network_port, Amount::from_sat(amount).to_btc(), addr);

                    let total_sleep = sleep_increment * sleep_multiplier.min(10 * 60);
                    log::info!(
                        "[{}] Next sync in {total_sleep:?} secs",
                        maker.config.network_port
                    );
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
                let proof_message = maker
                    .wallet()
                    .read()?
                    .generate_fidelity_proof(i, maker_address)?;

                // Convert to messages2::FidelityProof
                let highest_proof = crate::protocol::messages::FidelityProof {
                    bond: proof_message.bond,
                    cert_hash: *bitcoin::hashes::sha256d::Hash::from_bytes_ref(
                        proof_message.cert_hash.as_ref(),
                    ),
                    cert_sig: proof_message.cert_sig,
                };

                // sync and save the wallet data to disk
                log::info!("Sync at end:----setup_fidelity_bond----");
                maker.wallet().write()?.sync_and_save()?;

                // Store the fidelity proof in maker
                *maker.highest_fidelity_proof.write()? = Some(highest_proof.clone());

                return Ok(highest_proof);
            }
        }
    }

    Err(MakerError::General(
        "Failed to create fidelity bond due to shutdown",
    ))
}

/// Blocks until sufficient liquidity is available for taproot swaps.
fn check_swap_liquidity_taproot(maker: &Maker) -> Result<(), MakerError> {
    let sleep_incremental = 10;
    let mut sleep_duration = 0;
    while !maker.shutdown.load(Relaxed) {
        {
            #[cfg(feature = "integration-test")]
            thread::sleep(Duration::from_secs(5));
            log::info!("Sync at:----check_swap_liquidity----");
            let mut wallet = maker.wallet().write()?;
            wallet.sync_and_save()?;
            wallet.refresh_offer_maxsize_cache()?;
        }
        let offer_max_size = maker.wallet().read()?.store.offer_maxsize;
        let min_required = maker.config.min_swap_amount;
        if offer_max_size < min_required {
            let funding_addr = maker
                .wallet()
                .write()?
                .get_next_external_address(AddressType::P2TR)?;
            log::warn!(
                "[{}] Low taproot swap liquidity | Min: {} sats | Available: {} sats | Add Funds to: {:?}",
                maker.config.network_port,
                min_required,
                offer_max_size,
                funding_addr,
            );
            sleep_duration = (sleep_duration + sleep_incremental).min(10 * 60); // Capped at 1 Block interval
            log::info!(
                "[{}] Next liquidity check in {:?} seconds",
                maker.config.network_port,
                sleep_duration
            );

            std::thread::sleep(std::time::Duration::from_secs(sleep_duration));
        } else {
            log::info!(
                "[{}] Taproot swap liquidity ready: {} sats | Min: {} sats | Listening for requests.",
                maker.config.network_port,
                offer_max_size,
                min_required
            );
            break;
        }
    }

    Ok(())
}

/// Checks connection with Bitcoin Core for taproot operations
fn check_connection_with_core_taproot(maker: &Maker) -> Result<(), MakerError> {
    let wallet_read = maker.wallet().read()?;
    match wallet_read.rpc.get_block_count() {
        Ok(block_count) => {
            log::debug!(
                "[{}] Bitcoin Core connection OK, block height: {}",
                maker.config.network_port,
                block_count
            );
        }
        Err(e) => {
            log::error!(
                "[{}] Bitcoin Core connection failed: {}",
                maker.config.network_port,
                e
            );
            return Err(WalletError::Rpc(e).into());
        }
    }
    Ok(())
}

/// Handles a taproot client connection
fn handle_client_taproot(maker: &Arc<Maker>, stream: &mut TcpStream) -> Result<(), MakerError> {
    // Set socket to blocking mode for reliable message reading
    stream.set_nonblocking(false).map_err(NetError::IO)?;

    let peer_addr = stream.peer_addr().map_err(NetError::IO)?;
    let ip = peer_addr.ip().to_string();

    log::info!(
        "[{}] New taproot client connected: {} (port {})",
        maker.config.network_port,
        ip,
        peer_addr.port()
    );

    loop {
        // Check if we should shutdown
        if maker.shutdown.load(Relaxed) {
            log::info!(
                "[{}] Shutdown signal received, closing connection",
                maker.config.network_port
            );
            break;
        }

        // Read message from taker
        let message_bytes = match read_message(stream) {
            Ok(bytes) => bytes,
            Err(e) => {
                if let NetError::IO(io_err) = &e {
                    if io_err.kind() == ErrorKind::UnexpectedEof {
                        log::info!("[{}] Client {} disconnected", maker.config.network_port, ip);
                        break;
                    }
                }
                log::error!(
                    "[{}] Failed to read message from {}: {:?}",
                    maker.config.network_port,
                    ip,
                    e
                );
                break;
            }
        };

        log::debug!(
            "[{}] Received {} bytes from {}",
            maker.config.network_port,
            message_bytes.len(),
            ip
        );
        let message = match serde_cbor::from_slice::<TakerToMakerMessage>(&message_bytes) {
            Ok(msg) => {
                log::info!("[{}] <=== {}", maker.config.network_port, msg);
                msg
            }
            Err(e) => {
                log::error!(
                    "[{}] Failed to deserialize message from {}: {:?}",
                    maker.config.network_port,
                    ip,
                    e
                );
                break;
            }
        };

        let swap_id = match &message {
            TakerToMakerMessage::GetOffer(_) => Some(String::new()), // No swap id in GetOffer message
            TakerToMakerMessage::SwapDetails(msg) => Some(msg.id.clone()),
            TakerToMakerMessage::SendersContract(msg) => Some(msg.id.clone()),
            TakerToMakerMessage::PrivateKeyHandover(msg) => Some(msg.id.clone()),
        }
        .ok_or_else(|| MakerError::General("Message missing swap_id"))?;

        // Get or create connection state for this swap id
        let mut connection_state = {
            let ongoing_swaps = maker.ongoing_swap_state.lock()?;

            match &message {
                TakerToMakerMessage::GetOffer(_) => {
                    log::info!(
                        "[{}] Using temporary connection state for GetOffer",
                        maker.config.network_port,
                    );
                    // This connection state won't be persisted
                    ConnectionState::default()
                }
                TakerToMakerMessage::SwapDetails(_) => match ongoing_swaps.get(&swap_id) {
                    Some((state, _)) => {
                        log::info!(
                            "[{}] Protocol Violation: Found existing ConnectionState {:?} for SwapDetails for this SwapId:{},",
                            maker.config.network_port,
                            state,
                            swap_id,
                        );
                        return Err(MakerError::General(
                            "Protocol violation: Found duplicate Swap Details message",
                        ));
                    }
                    None => {
                        log::info!(
                            "[{}] Creating new connection state for SwapDetails",
                            maker.config.network_port
                        );
                        ConnectionState::default()
                    }
                },
                _ => match ongoing_swaps.get(&swap_id) {
                    Some((state, _)) => state.clone(),
                    None => {
                        log::info!(
                                "[{}] No connection state found for swap_id={:?}. GetOffer must be sent first.",
                                maker.config.network_port,
                                &swap_id
                            );
                        return Err(MakerError::General("No connection state found"));
                    }
                },
            }
        };

        // Handle the message using taproot handlers
        let response = match handle_message_taproot(maker, &mut connection_state, message) {
            Ok(response) => response,
            Err(e) => {
                log::error!(
                    "[{}] Error handling message from {}: {:?}",
                    maker.config.network_port,
                    ip,
                    e
                );

                // Check if this is a behavior-triggered error
                match &e {
                    MakerError::General(msg) if msg.contains("behavior") => {
                        log::info!(
                            "[{}] Behavior-triggered disconnection",
                            maker.config.network_port
                        );
                    }
                    _ => {}
                }
                break;
            }
        };

        // Persist connection state after sending AckResponse message only.
        if !matches!(response, Some(MakerToTakerMessage::RespOffer(_))) {
            let mut ongoing_swaps = maker.ongoing_swap_state.lock()?;
            log::info!(
                "[{}] Persisting connection state for {}: swap_amount={}, my_privkey_is_some={}",
                maker.config.network_port,
                ip,
                connection_state.swap_amount,
                connection_state.incoming_contract.my_privkey.is_some()
            );
            ongoing_swaps.insert(swap_id.clone(), (connection_state.clone(), Instant::now()));
        }
        // Send response if we have one (only applies to taker messages)
        if let Some(response_msg) = response {
            log::info!("[{}] ===> {}", maker.config.network_port, response_msg);

            if let Err(e) = send_message(stream, &response_msg) {
                log::error!(
                    "[{}] Failed to send response to {}: {:?}",
                    maker.config.network_port,
                    ip,
                    e
                );
                break;
            }

            log::info!(
                "[{}] Successfully sent response to {}",
                maker.config.network_port,
                ip
            );

            if let MakerToTakerMessage::PrivateKeyHandover(_) = &response_msg {
                // Swap completed successfully - remove outgoing swapcoin and ongoing swap state
                log::info!(
                    "[{}] Swap completed successfully with {}, removing from ongoing swaps",
                    maker.config.network_port,
                    ip
                );

                // Remove the outgoing swapcoin now that PrivateKeyHandover was sent
                let outgoing_txid = connection_state
                    .outgoing_contract
                    .contract_tx
                    .compute_txid();
                {
                    let mut wallet = maker.wallet.write()?;
                    wallet.remove_outgoing_swapcoin_v2(&outgoing_txid);
                    log::info!(
                        "[{}] Removed outgoing swapcoin {} after PrivateKeyHandover sent",
                        maker.config.network_port,
                        outgoing_txid
                    );
                    wallet.save_to_disk()?;
                }

                let mut ongoing_swaps = maker.ongoing_swap_state.lock()?;
                ongoing_swaps.remove(&swap_id);
                // Exit loop - swap is complete
                break;
            }
        }
    }

    log::info!(
        "[{}] Client {} session ended",
        maker.config.network_port,
        ip
    );
    Ok(())
}

/// Starts the taproot maker server
pub fn start_maker_server_taproot(maker: Arc<Maker>) -> Result<(), MakerError> {
    let network_port = maker.config.network_port;
    log::info!("[{network_port}] Starting taproot coinswap maker server",);

    // Set up network
    let maker_address = network_bootstrap_taproot(maker.clone())?;

    log::info!(
        "[{network_port}] Taproot maker initialized - Address: {}",
        maker_address,
    );
    // Check swap liquidity before spawning the threads
    check_swap_liquidity_taproot(&maker)?;
    // Start idle connection checker thread
    let maker_clone_idle = maker.clone();
    let idle_checker_handle = thread::Builder::new()
        .name("idle-checker-taproot".to_string())
        .spawn(move || {
            if let Err(e) = check_for_idle_states(maker_clone_idle) {
                log::error!("Taproot idle checker thread error: {:?}", e);
            }
        })?;
    maker.thread_pool.add_thread(idle_checker_handle);

    // Start contract broadcast watcher thread
    let maker_clone_watcher = maker.clone();
    let contract_watcher_handle = thread::Builder::new()
        .name("contract-watcher-taproot".to_string())
        .spawn(move || {
            if let Err(e) = check_for_broadcasted_contracts(maker_clone_watcher) {
                log::error!("Taproot contract watcher thread error: {:?}", e);
            }
        })?;
    maker.thread_pool.add_thread(contract_watcher_handle);

    // Start liquidity monitoring
    let maker_clone_liquidity = maker.clone();
    let liquidity_handle = thread::Builder::new()
        .name("liquidity-monitor-taproot".to_string())
        .spawn(move || {
            let mut counter = 0;
            let heartbeat_secs = HEART_BEAT_INTERVAL.as_secs().max(1);
            let check_interval = (300u64 / heartbeat_secs).max(1); // Check every 5 minutes

            loop {
                if maker_clone_liquidity.shutdown.load(Relaxed) {
                    break;
                }

                if counter % check_interval == 0 {
                    if let Err(e) = check_swap_liquidity_taproot(&maker_clone_liquidity) {
                        log::error!("Liquidity check error: {:?}", e);
                    }
                }

                counter += 1;
                sleep(HEART_BEAT_INTERVAL);
            }
        })?;
    maker.thread_pool.add_thread(liquidity_handle);

    // fidelity renewal thread
    let maker_clone_fidelity = maker.clone();
    let maker_addr_clone = maker_address.clone();
    let fidelity_handle = thread::Builder::new()
        .name("fidelity-monitor-taproot".to_string())
        .spawn(move || {
            let mut counter = 0u64;
            let heartbeat_secs = HEART_BEAT_INTERVAL.as_secs().max(1);
            let check_interval =
                (FIDELITY_BOND_UPDATE_INTERVAL as u64 / heartbeat_secs).max(1);

            loop {
                if maker_clone_fidelity.shutdown.load(Relaxed) {
                    break;
                }

                let swaps_empty = maker_clone_fidelity
                    .ongoing_swap_state
                    .lock()
                    .map(|s| s.is_empty())
                    .unwrap_or(false);

                let check_is_due = counter.is_multiple_of(check_interval);

                if check_is_due && swaps_empty {
                    log::info!(
                        "[{}] Running periodic fidelity bond check",
                        maker_clone_fidelity.config.network_port
                    );
                    if let Err(e) = manage_fidelity_bonds_taproot(
                        maker_clone_fidelity.clone(),
                        &maker_addr_clone,
                        false,
                    ) {
                        log::error!("Fidelity bond renewal error: {:?}", e);
                    }
                }

                if swaps_empty || !check_is_due {
                    counter += 1;
                }

                sleep(HEART_BEAT_INTERVAL);
            }
        })?;
    maker.thread_pool.add_thread(fidelity_handle);

    // Start Bitcoin Core connection monitoring
    let maker_clone_core = maker.clone();
    let core_monitor_handle = thread::Builder::new()
        .name("core-monitor-taproot".to_string())
        .spawn(move || {
            let mut counter = 0;
            let heartbeat_secs = HEART_BEAT_INTERVAL.as_secs().max(1);
            let check_interval = (RPC_PING_INTERVAL as u64 / heartbeat_secs).max(1); // Check every 9 seconds

            loop {
                if maker_clone_core.shutdown.load(Relaxed) {
                    break;
                }

                if counter % check_interval == 0 {
                    if let Err(e) = check_connection_with_core_taproot(&maker_clone_core) {
                        log::error!("Bitcoin Core connection check error: {:?}", e);
                    }
                }

                counter += 1;
                sleep(HEART_BEAT_INTERVAL);
            }
        })?;
    maker.thread_pool.add_thread(core_monitor_handle);

    // Start RPC server
    let maker_clone_rpc = maker.clone();
    let rpc_handle = thread::Builder::new()
        .name("rpc-server-taproot".to_string())
        .spawn(move || {
            if let Err(e) = start_rpc_server(maker_clone_rpc) {
                log::error!("Taproot RPC server error: {:?}", e);
            }
        })?;
    maker.thread_pool.add_thread(rpc_handle);

    // Check for unfinished swapcoins from previous runs and start recovery if needed
    {
        let (inc, out) = maker.wallet.read()?.find_unfinished_swapcoins_v2();
        if !inc.is_empty() || !out.is_empty() {
            log::info!(
                "[{network_port}] Incomplete taproot swaps detected ({} incoming, {} outgoing). Starting recovery.",
                inc.len(),
                out.len()
            );
            crate::maker::api2::restore_broadcasted_contracts_on_reboot_v2(&maker)?;
        }
    }

    // Mark setup as complete
    maker.is_setup_complete.store(true, Relaxed);
    log::info!("[{network_port}] Taproot maker setup completed",);

    // Start listening for P2P connections
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, network_port))?;
    log::info!("[{network_port}] Taproot maker server listening on port {network_port}",);

    // Set listener to non-blocking mode to allow periodic shutdown checks
    listener.set_nonblocking(true)?;

    // Main server loop
    while !maker.shutdown.load(Relaxed) {
        match listener.accept() {
            Ok((mut stream, _)) => {
                if let Err(e) = handle_client_taproot(&maker, &mut stream) {
                    log::error!("[{network_port}] Error Handling client request {e:?}");
                }
            }

            Err(e) => {
                if e.kind() != ErrorKind::WouldBlock {
                    log::error!("[{network_port}] Error accepting incoming connection: {e:?}");
                }
            }
        };
        // Sleep briefly to avoid busy waiting
        sleep(HEART_BEAT_INTERVAL);
    }

    log::info!(
        "[{}] Taproot maker server shutting down",
        maker.config.network_port
    );

    maker.watch_service.shutdown();

    // Join all threads
    maker.thread_pool.join_all_threads()?;

    log::info!("sync at:----Taproot server shutdown----");
    maker.wallet().write()?.sync_and_save()?;
    log::info!(
        "[{}] Taproot maker server stopped",
        maker.config.network_port
    );
    Ok(())
}

//! The Coinswap Maker Server for Taproot protocol.
//!
//! This module includes all server side code for the coinswap maker using the new taproot protocol.
//! The server maintains the thread pool for P2P Connection, Watchtower, Bitcoin Backend, and RPC Client Request.
//! It uses the new message protocol (messages2) and integrates with api2.rs and handlers2.rs.
//! The server listens at two ports: 6102 for P2P, and 6103 for RPC Client requests.

use bitcoin::Amount;
use bitcoind::bitcoincore_rpc::RpcApi;
use nostr::{
    event::{EventBuilder, Kind},
    key::{Keys, SecretKey},
    message::{ClientMessage, RelayMessage},
    util::JsonUtil,
};
use std::{
    io::ErrorKind,
    net::{Ipv4Addr, TcpListener, TcpStream},
    sync::{atomic::Ordering::Relaxed, Arc},
    thread::{self, sleep},
    time::{Duration, Instant},
};
use tungstenite::Message;

pub(crate) use super::api2::{Maker, RPC_PING_INTERVAL};

use crate::{
    error::NetError,
    maker::{
        api2::{check_for_broadcasted_contracts, check_for_idle_states, ConnectionState},
        handlers2::handle_message_taproot,
        rpc::start_rpc_server,
    },
    protocol::messages2::{FidelityProof, MakerToTakerMessage, TakerToMakerMessage},
    utill::{
        get_tor_hostname, read_message, send_message, COINSWAP_KIND, HEART_BEAT_INTERVAL,
        MIN_FEE_RATE, NOSTR_RELAYS,
    },
    wallet::{AddressType, WalletError},
};

use crate::maker::error::MakerError;

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

    manage_fidelity_bonds_taproot(maker.as_ref(), &maker_address)?;

    Ok(maker_address)
}

/// Setup's maker fidelity
fn manage_fidelity_bonds_taproot(maker: &Maker, maker_addr: &str) -> Result<(), MakerError> {
    // Redeem expired fidelity bonds first
    maker
        .wallet()
        .write()?
        .redeem_expired_fidelity_bonds(AddressType::P2TR)?;

    // Create or get existing fidelity proof for taproot maker
    let fidelity_proof = setup_fidelity_bond_taproot(maker, maker_addr)?;

    broadcast_bond_on_nostr(fidelity_proof)?;

    Ok(())
}

// ##TODO: Make this part of nostr module and improve error handing
// ##TODO: Try retry in case relay doesn't accept the event
fn broadcast_bond_on_nostr(fidelity: FidelityProof) -> Result<(), MakerError> {
    let outpoint = fidelity.bond.outpoint;
    let content = format!("{}:{}", outpoint.txid, outpoint.vout);

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

/// Ensures the wallet has a valid fidelity bond for taproot operations.
/// This follows the same pattern as the regular maker setup_fidelity_bond function.
fn setup_fidelity_bond_taproot(
    maker: &Maker,
    maker_address: &str,
) -> Result<crate::protocol::messages2::FidelityProof, MakerError> {
    use crate::wallet::WalletError;
    use bitcoin::absolute::LockTime;
    use std::thread;

    let highest_index = maker.wallet().read()?.get_highest_fidelity_index()?;

    if let Some(i) = highest_index {
        let wallet_read = maker.wallet().read()?;
        let bond = wallet_read.store.fidelity_bond.get(&i).unwrap();

        let current_height = wallet_read
            .rpc
            .get_block_count()
            .map_err(WalletError::Rpc)? as u32;

        let proof_message = maker
            .wallet()
            .read()?
            .generate_fidelity_proof(i, maker_address)?;

        // Convert to messages2::FidelityProof
        let highest_proof = crate::protocol::messages2::FidelityProof {
            bond: proof_message.bond.clone(),
            cert_hash: *bitcoin::hashes::sha256::Hash::from_bytes_ref(
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
            wallet_read.calculate_bond_value(bond)?.to_sat()
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
    let locktime = if cfg!(feature = "integration-test") {
        LockTime::from_height(current_height + 950).map_err(WalletError::Locktime)?
    } else {
        LockTime::from_height(maker.config.fidelity_timelock + current_height)
            .map_err(WalletError::Locktime)?
    };

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
                let highest_proof = crate::protocol::messages2::FidelityProof {
                    bond: proof_message.bond,
                    cert_hash: *bitcoin::hashes::sha256::Hash::from_bytes_ref(
                        proof_message.cert_hash.as_ref(),
                    ),
                    cert_sig: proof_message.cert_sig,
                };

                // sync and save the wallet data to disk
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

/// Checks swap liquidity for taproot swaps
fn check_swap_liquidity_taproot(maker: &Maker) -> Result<(), MakerError> {
    let wallet_read = maker.wallet().read()?;
    let balances = wallet_read.get_balances()?;

    let swap_balance = balances.spendable;
    let ongoing_swaps_count = maker.ongoing_swap_state.lock()?.len();

    if swap_balance == Amount::ZERO {
        log::warn!(
            "[{}] No spendable balance available for taproot swaps",
            maker.config.network_port
        );
    } else {
        log::info!(
            "[{}] Taproot swap liquidity: {} sats, {} ongoing swaps",
            maker.config.network_port,
            swap_balance.to_sat(),
            ongoing_swaps_count
        );
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
                log::debug!(
                    "[{}] Successfully decoded message: {:?}",
                    maker.config.network_port,
                    msg
                );
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
            TakerToMakerMessage::GetOffer(msg) => Some(msg.id.clone()),
            TakerToMakerMessage::SwapDetails(msg) => Some(msg.id.clone()),
            TakerToMakerMessage::SendersContract(msg) => Some(msg.id.clone()),
            TakerToMakerMessage::PrivateKeyHandover(msg) => msg.id.clone(),
        }
        .ok_or_else(|| MakerError::General("Message missing swap_id"))?;

        // Get or create connection state for this swap id
        let mut connection_state = {
            let mut ongoing_swaps = maker.ongoing_swap_state.lock()?;

            match &message {
                TakerToMakerMessage::GetOffer(_) => {
                    // TODO: Right now sync_offerbook is sending GetOffer with an empty string swap_id
                    // Refactor to not create a connection state when swap id is an empty string
                    // Just return the Offer
                    let (state, _) = ongoing_swaps.entry(swap_id.clone()).or_insert_with(|| {
                        log::info!(
                            "[{}] Creating new connection state for {:?}",
                            maker.config.network_port,
                            &swap_id
                        );
                        (ConnectionState::default(), Instant::now())
                    });
                    state.clone()
                }
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
        {
            let mut ongoing_swaps = maker.ongoing_swap_state.lock()?;
            log::info!(
                "[{}] Saving connection state for {}: swap_amount={}, my_privkey_is_some={}",
                maker.config.network_port,
                ip,
                connection_state.swap_amount,
                connection_state.incoming_contract.my_privkey.is_some()
            );
            ongoing_swaps.insert(swap_id.clone(), (connection_state.clone(), Instant::now()));
        }
        // Send response if we have one (only applies to taker messages)
        if let Some(response_msg) = response {
            log::info!("[{}] Sending response", maker.config.network_port,);

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
    log::info!(
        "[{}] Starting taproot coinswap maker server",
        maker.config.network_port
    );

    // Set up network
    let maker_address = network_bootstrap_taproot(maker.clone())?;

    log::info!(
        "[{}] Taproot maker initialized - Address: {}",
        maker.config.network_port,
        maker_address,
    );

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
            let check_interval = 300 / HEART_BEAT_INTERVAL.as_secs(); // Check every 5 minutes

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

    // Start Bitcoin Core connection monitoring
    let maker_clone_core = maker.clone();
    let core_monitor_handle = thread::Builder::new()
        .name("core-monitor-taproot".to_string())
        .spawn(move || {
            let mut counter = 0;
            let check_interval = RPC_PING_INTERVAL as u64 / HEART_BEAT_INTERVAL.as_secs(); // Check every 9 seconds

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
                "[{}] Incomplete taproot swaps detected ({} incoming, {} outgoing). Starting recovery.",
                maker.config.network_port,
                inc.len(),
                out.len()
            );
            crate::maker::api2::restore_broadcasted_contracts_on_reboot_v2(&maker)?;
        }
    }

    // Mark setup as complete
    maker.is_setup_complete.store(true, Relaxed);
    log::info!(
        "[{}] Taproot maker setup completed",
        maker.config.network_port
    );

    // Start listening for P2P connections
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, maker.config.network_port))?;
    log::info!(
        "[{}] Taproot maker server listening on port {}",
        maker.config.network_port,
        maker.config.network_port
    );

    // Set listener to non-blocking mode to allow periodic shutdown checks
    listener.set_nonblocking(true)?;

    // Main server loop
    while !maker.shutdown.load(Relaxed) {
        match listener.accept() {
            Ok((mut stream, addr)) => {
                log::info!(
                    "[{}] New client connection from: {}",
                    maker.config.network_port,
                    addr
                );
                let maker_clone = maker.clone();
                let client_handle = thread::Builder::new()
                    .name(format!("client-{}", addr))
                    .spawn(move || {
                        if let Err(e) = handle_client_taproot(&maker_clone, &mut stream) {
                            log::error!("Client handler error: {:?}", e);
                        }
                    })?;
                maker.thread_pool.add_thread(client_handle);
            }
            Err(e) => {
                if e.kind() == ErrorKind::WouldBlock {
                    // No incoming connections, continue loop
                } else {
                    log::error!(
                        "[{}] Failed to accept connection: {:?}",
                        maker.config.network_port,
                        e
                    );
                }
            }
        }

        // Sleep briefly to avoid busy waiting
        sleep(HEART_BEAT_INTERVAL);
    }

    log::info!(
        "[{}] Taproot maker server shutting down",
        maker.config.network_port
    );

    // Join all threads
    maker.thread_pool.join_all_threads()?;

    log::info!(
        "[{}] Taproot maker server stopped",
        maker.config.network_port
    );
    Ok(())
}

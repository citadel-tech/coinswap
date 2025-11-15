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
    sync::{atomic::Ordering::Relaxed, Arc},
    thread::{self, sleep},
    time::{Duration, Instant},
};

pub(crate) use super::api2::{Maker, RPC_PING_INTERVAL};

use crate::{
    error::NetError,
    maker::{
        api2::{check_for_idle_states, ConnectionState},
        handlers2::handle_message_taproot,
        rpc::start_rpc_server,
    },
    protocol::messages2::{MakerToTakerMessage, TakerToMakerMessage},
    utill::{get_tor_hostname, read_message, send_message, HEART_BEAT_INTERVAL, MIN_FEE_RATE},
    wallet::WalletError,
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
    maker.wallet().write()?.redeem_expired_fidelity_bonds()?;

    // Create or get existing fidelity proof for taproot maker
    let _ = setup_fidelity_bond_taproot(maker, maker_addr)?;

    let network_port = maker.config.network_port;
    log::info!("[{network_port}] Taproot maker initialized - Address: {maker_addr}");
    log::info!("[{network_port}] Connection ended.");

    Ok(())
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
        maker.wallet().write()?.sync_no_fail();

        let fidelity_result = maker.wallet().write()?.create_fidelity(
            amount,
            locktime,
            Some(maker_address.as_bytes()),
            MIN_FEE_RATE,
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
                    let addr = maker.wallet().write()?.get_next_external_address()?;

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
                maker.wallet().write()?.sync_no_fail();
                maker.wallet().read()?.save_to_disk()?;

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

    // Get or create connection state for this IP
    let mut connection_state = {
        let mut ongoing_swaps = maker.ongoing_swap_state.lock()?;
        let (state, _) = ongoing_swaps.entry(ip.clone()).or_insert_with(|| {
            log::info!(
                "[{}] Creating new connection state for {}",
                maker.config.network_port,
                ip
            );
            (ConnectionState::default(), Instant::now())
        });

        log::info!(
            "[{}] Retrieved connection state for {}: swap_amount={}",
            maker.config.network_port,
            ip,
            state.swap_amount
        );

        state.clone()
    };

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

        // Handle the message using taproot handlers
        let response = match handle_message_taproot(maker, &mut connection_state, message) {
            Ok(response) => {
                // Save connection state immediately after successful message handling
                {
                    let mut ongoing_swaps = maker.ongoing_swap_state.lock()?;
                    ongoing_swaps.insert(ip.clone(), (connection_state.clone(), Instant::now()));
                    log::debug!(
                        "[{}] Saved connection state for {} after successful message handling",
                        maker.config.network_port,
                        ip
                    );
                }
                response
            }
            Err(e) => {
                log::error!(
                    "[{}] Error handling message from {}: {:?}",
                    maker.config.network_port,
                    ip,
                    e
                );

                // Always save connection state even if there was an error
                {
                    let mut ongoing_swaps = maker.ongoing_swap_state.lock()?;
                    ongoing_swaps.insert(ip.clone(), (connection_state.clone(), Instant::now()));
                }

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

            // For certain message types, close the connection after responding
            match &response_msg {
                MakerToTakerMessage::RespOffer(_) => {
                    // Keep connection open for SwapDetails
                }
                MakerToTakerMessage::AckResponse(_) => {
                    // Keep connection open for SendersContract
                    log::debug!(
                        "[{}] Keeping connection open after AckResponse for SendersContract",
                        maker.config.network_port
                    );
                }
                MakerToTakerMessage::SenderContractFromMaker(_) => {
                    // Keep connection open for SpendingTxAndReceiverNonce
                }
                MakerToTakerMessage::PrivateKeyHandover(_) => {
                    // Keep connection open for PartialSigAndSendersNonce
                }
            }
        }

        // Update the connection state in the maker
        {
            let mut ongoing_swaps = maker.ongoing_swap_state.lock()?;
            ongoing_swaps.insert(ip.clone(), (connection_state.clone(), Instant::now()));
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

    let maker_clone_idle = maker.clone();
    let idle_checker_handle = thread::Builder::new()
        .name("idle-checker-taproot".to_string())
        .spawn(move || {
            if let Err(e) = check_for_idle_states(maker_clone_idle) {
                log::error!("Taproot idle checker thread error: {:?}", e);
            }
        })?;
    maker.thread_pool.add_thread(idle_checker_handle);

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

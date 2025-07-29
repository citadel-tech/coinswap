//! The Coinswap Maker Server for Taproot protocol.
//!
//! This module includes all server side code for the coinswap maker using the new taproot protocol.
//! The server maintains the thread pool for P2P Connection, Watchtower, Bitcoin Backend, and RPC Client Request.
//! It uses the new message protocol (messages2) and integrates with api2.rs and handlers2.rs.
//! The server listens at two ports: 6102 for P2P, and 6103 for RPC Client requests.

use bitcoin::Amount;
use bitcoind::bitcoincore_rpc::RpcApi;
use socks::Socks5Stream;
use std::{
    io::ErrorKind,
    net::{Ipv4Addr, TcpListener, TcpStream},
    sync::{atomic::Ordering::Relaxed, Arc},
    thread::{self, sleep},
    time::{Duration, Instant},
};

use crate::utill::get_tor_hostname;

pub(crate) use super::api2::{Maker, RPC_PING_INTERVAL};

use crate::{
    error::NetError,
    maker::{
        api2::{check_for_broadcasted_contracts, check_for_idle_states, ConnectionState},
        handlers2::handle_message_taproot,
        rpc2::start_rpc_server_taproot,
    },
    protocol::{
        messages::{DnsMetadata, DnsRequest, DnsResponse}, // Use regular DNS messages
        messages2::{MakerToTakerMessage, TakerToMakerMessage},
    },
    utill::{read_message, send_message, ConnectionType, DEFAULT_TX_FEE_RATE, HEART_BEAT_INTERVAL},
    wallet::WalletError,
};

use crate::maker::error::MakerError;

/// Fetches the Maker and DNS address, and sends maker address to the DNS server for taproot swaps.
/// Depending upon ConnectionType and test/prod environment, different maker address and DNS address are returned.
/// Return the Maker address and the DNS address.
fn network_bootstrap_taproot(maker: Arc<Maker>) -> Result<(String, String), MakerError> {
    let maker_port = maker.config.network_port;
    let (maker_address, dns_address) = match maker.config.connection_type {
        ConnectionType::CLEARNET => {
            let maker_address = format!("127.0.0.1:{maker_port}");
            let dns_address = if cfg!(feature = "integration-test") {
                format!("127.0.0.1:{}", 8080)
            } else {
                maker.config.dns_address.clone()
            };

            (maker_address, dns_address)
        }
        ConnectionType::TOR => {
            let maker_hostname = get_tor_hostname(
                maker.get_data_dir(),
                maker.config.control_port,
                maker.config.network_port,
                &maker.config.tor_auth_password,
            )?;
            let maker_address = format!("{}:{}", maker_hostname, maker.config.network_port);

            let dns_address = maker.config.dns_address.clone();
            (maker_address, dns_address)
        }
    };

    // Track and update unconfirmed fidelity bonds
    maker
        .as_ref()
        .track_and_update_unconfirmed_fidelity_bonds()?;

    // Register our taproot-capable maker with the DNS
    manage_fidelity_bonds_and_update_dns_taproot(maker.as_ref(), &maker_address, &dns_address)?;

    Ok((maker_address, dns_address))
}

/// Manages the maker's fidelity bonds and ensures the DNS server is updated with the latest bond proof and maker address.
/// This version is adapted for taproot protocol but follows the same DNS registration pattern as regular makers.
fn manage_fidelity_bonds_and_update_dns_taproot(
    maker: &Maker,
    maker_addr: &str,
    dns_addr: &str,
) -> Result<(), MakerError> {
    // Redeem expired fidelity bonds first
    maker
        .get_wallet()
        .write()?
        .redeem_expired_fidelity_bonds()?;

    // Create or get existing fidelity proof for taproot maker
    let proof = setup_fidelity_bond_taproot(maker, maker_addr)?;

    let dns_metadata = DnsMetadata {
        url: maker_addr.to_string(),
        proof,
    };

    let request = DnsRequest::Post {
        metadata: dns_metadata,
    };

    let network_port = maker.config.network_port;

    log::info!("[{network_port}] Connecting to DNS: {dns_addr} (taproot)");

    while !maker.shutdown.load(Relaxed) {
        let stream = match maker.config.connection_type {
            ConnectionType::CLEARNET => TcpStream::connect(dns_addr),
            ConnectionType::TOR => {
                Socks5Stream::connect(format!("127.0.0.1:{}", maker.config.socks_port), dns_addr)
                    .map(|s| s.into_inner())
            }
        };

        match stream {
            Ok(mut stream) => match send_message(&mut stream, &request) {
                Ok(_) => match read_message(&mut stream) {
                    Ok(dns_msg_bytes) => {
                        match serde_cbor::from_slice::<DnsResponse>(&dns_msg_bytes) {
                            Ok(dns_msg) => match dns_msg {
                                DnsResponse::Ack => {
                                    log::info!("[{network_port}] <=== {dns_msg}");
                                    log::info!( "[{network_port}] Successfully sent our taproot address and fidelity proof to DNS at {dns_addr}");
                                    break;
                                }
                                DnsResponse::Nack(reason) => {
                                    log::error!("<=== DNS Nack: {reason}")
                                }
                            },
                            Err(e) => {
                                log::warn!("CBOR deserialization failed: {e} | Reattempting...")
                            }
                        }
                    }
                    Err(e) => {
                        if let NetError::IO(e) = e {
                            if e.kind() == ErrorKind::UnexpectedEof {
                                log::info!("[{}] Connection ended.", maker.config.network_port);
                                break;
                            } else {
                                log::error!(
                                    "[{}] DNS Connection Error: {}",
                                    maker.config.network_port,
                                    e
                                );
                            }
                        }
                    }
                },
                Err(e) => log::warn!(
                    "[{network_port}] Failed to send request to DNS : {e} | reattempting..."
                ),
            },
            Err(e) => log::warn!(
                "[{network_port}] Failed to establish TCP connection with DNS : {e} | reattempting..."
            ),
        }

        thread::sleep(HEART_BEAT_INTERVAL);
    }

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

    let highest_index = maker.get_wallet().read()?.get_highest_fidelity_index()?;

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
            "[{}] Using existing fidelity bond at outpoint {} | index {} | Amount {:?} sats | Remaining Timelock for expiry : {:?} Blocks | Current Bond Value : {:?} sats",
            maker.config.network_port,
            highest_proof.bond.outpoint,
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
        .get_wallet()
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
        maker.get_wallet().write()?.sync_no_fail();

        let fidelity_result =
            maker
                .get_wallet()
                .write()?
                .create_fidelity(amount, locktime, None, DEFAULT_TX_FEE_RATE);

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
                    let addr = maker.get_wallet().write()?.get_next_external_address()?;

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
                let highest_proof = maker
                    .get_wallet()
                    .read()?
                    .generate_fidelity_proof(i, maker_address)?;

                // sync and save the wallet data to disk
                maker.get_wallet().write()?.sync_no_fail();
                maker.get_wallet().read()?.save_to_disk()?;

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
    let wallet_read = maker.get_wallet().read()?;
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
    let wallet_read = maker.get_wallet().read()?;
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
    let peer_addr = stream.peer_addr().map_err(NetError::IO)?;
    let ip = peer_addr.ip().to_string();

    log::info!(
        "[{}] New taproot client connected: {}",
        maker.config.network_port,
        ip
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

        // Deserialize the message
        let message: TakerToMakerMessage = match serde_cbor::from_slice(&message_bytes) {
            Ok(msg) => {
                log::debug!(
                    "[{}] Received message: {:?}",
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

        // Send response if we have one
        if let Some(response_msg) = response {
            log::info!(
                "[{}] Sending response: {:?}",
                maker.config.network_port,
                response_msg
            );

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
                MakerToTakerMessage::NoncesPartialSigsAndSpendingTx(_) => {
                    // Keep connection open for PartialSigAndSendersNonce
                }
                _ => {
                    // For other responses, keep connection open
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

    // Set up network and DNS registration
    let (maker_address, dns_address) = network_bootstrap_taproot(maker.clone())?;

    log::info!(
        "[{}] Taproot maker initialized - Address: {}, DNS: {}",
        maker.config.network_port,
        maker_address,
        dns_address
    );

    // Start monitoring threads
    let maker_clone_watchtower = maker.clone();
    let watchtower_handle = thread::Builder::new()
        .name("watchtower-taproot".to_string())
        .spawn(move || {
            if let Err(e) = check_for_broadcasted_contracts(maker_clone_watchtower) {
                log::error!("Taproot watchtower thread error: {:?}", e);
            }
        })?;
    maker.thread_pool.add_thread(watchtower_handle);

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
            if let Err(e) = start_rpc_server_taproot(maker_clone_rpc) {
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

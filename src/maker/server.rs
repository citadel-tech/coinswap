//! The Coinswap Maker Server.
//!
//! This module includes all server side code for the coinswap maker.
//! The server maintains the thread pool for P2P Connection, Watchtower, Bitcoin Backend and RPC Client Request.
//! The server listens at two port 6102 for P2P, and 6103 for RPC Client request.

use crate::protocol::messages::FidelityProof;
use bitcoin::{absolute::LockTime, Amount};
use bitcoind::bitcoincore_rpc::RpcApi;
use socks::Socks5Stream;
use std::{
    io::ErrorKind,
    net::{Ipv4Addr, TcpListener, TcpStream},
    sync::{atomic::Ordering::Relaxed, Arc},
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
            FIDELITY_BOND_DNS_UPDATE_INTERVAL, SWAP_LIQUIDITY_CHECK_INTERVAL,
        },
        handlers::handle_message,
        rpc::start_rpc_server,
    },
    protocol::messages::{DnsMetadata, DnsRequest, DnsResponse, TakerToMakerMessage},
    utill::{read_message, send_message, ConnectionType, DEFAULT_TX_FEE_RATE, HEART_BEAT_INTERVAL},
    wallet::WalletError,
};

use crate::maker::error::MakerError;

/// Fetches the Maker and DNS address, and sends maker address to the DNS server.
/// Depending upon ConnectionType and test/prod environment, different maker address and DNS addresses are returned.
/// Return the Maker address and the DNS address.
fn network_bootstrap(maker: Arc<Maker>) -> Result<(String, String), MakerError> {
    let maker_port = maker.config.network_port;
    let (maker_address, dns_address) = match maker.config.connection_type {
        ConnectionType::CLEARNET => {
            let maker_address = format!("127.0.0.1:{}", maker_port);
            let dns_address = if cfg!(feature = "integration-test") {
                format!("127.0.0.1:{}", 8080)
            } else {
                maker.config.directory_server_address.clone()
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

            let dns_address = maker.config.directory_server_address.clone();
            (maker_address, dns_address)
        }
    };

    maker
        .as_ref()
        .track_and_update_unconfirmed_fidelity_bonds()?;

    manage_fidelity_bonds_and_update_dns(maker.as_ref(), &maker_address, &dns_address)?;

    Ok((maker_address, dns_address))
}

/// Manages the maker's fidelity bonds and ensures the DNS server is updated with the latest bond proof and maker address.
///
/// It performs the following operations:
/// 1. Redeems all expired fidelity bonds in the maker's wallet, if any are found.
/// 2. Creates a new fidelity bond if no valid bonds remain after redemption.
/// 3. Sends a POST request to the DNS server containing the maker's address and the proof of the fidelity bond
///    with the highest value.
fn manage_fidelity_bonds_and_update_dns(
    maker: &Maker,
    maker_addr: &str,
    dns_addr: &str,
) -> Result<(), MakerError> {
    maker.wallet.write()?.redeem_expired_fidelity_bonds()?;

    let proof = setup_fidelity_bond(maker, maker_addr)?;

    let dns_metadata = DnsMetadata {
        url: maker_addr.to_string(),
        proof,
    };

    let request = DnsRequest::Post {
        metadata: dns_metadata,
    };

    let network_port = maker.config.network_port;

    log::info!("[{}] Connecting to DNS: {}", network_port, dns_addr);

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
                                    log::info!("[{}] <=== {}", network_port, dns_msg);
                                    log::info!( "[{}] Successfully sent our address and fidelity proof to DNS at {}",network_port, dns_addr);
                                    break;
                                }
                                DnsResponse::Nack(reason) => {
                                    log::error!("<=== DNS Nack: {}", reason)
                                }
                            },
                            Err(e) => {
                                log::warn!("CBOR deserialization failed: {} | Reattempting...", e)
                            }
                        }
                    }
                    Err(e) => {
                        if let NetError::IO(e) = e {
                            if e.kind() == ErrorKind::UnexpectedEof {
                                log::info!("[{}] Connection ended.", maker.config.network_port);
                                break;
                            } else {
                                // For any other errors, report them
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
                    "[{}] Failed to send request to DNS : {} | reattempting...",
                    network_port,
                    e
                ),
            },
            Err(e) => log::warn!(
                "[{}] Failed to establish TCP connection with DNS : {} | reattempting...",
                network_port,
                e
            ),
        }

        thread::sleep(HEART_BEAT_INTERVAL);
    }

    Ok(())
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
        let (bond, _) = wallet_read.store.fidelity_bond.get(&i).unwrap();

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
            maker.get_wallet().write()?.sync_no_fail();

            let fidelity_result =
                maker
                    .get_wallet()
                    .write()?
                    .create_fidelity(amount, locktime, DEFAULT_TX_FEE_RATE);

            match fidelity_result {
                // Wait for sufficient fund to create fidelity bond.
                // Hard error if fidelity still can't be created.
                Err(e) => {
                    if let WalletError::InsufficientFund {
                        available,
                        required,
                    } = e
                    {
                        log::warn!("Insufficient fund to create fidelity bond.");
                        let amount = required - available;
                        let addr = maker.get_wallet().write()?.get_next_external_address()?;

                        log::info!("Send at least {:.8} BTC to {:?} | If you send extra, that will be added to your wallet balance", Amount::from_sat(amount).to_btc(), addr);

                        let total_sleep = sleep_increment * sleep_multiplier.min(10 * 60);
                        log::info!("Next sync in {:?} secs", total_sleep);
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
                    maker.get_wallet().write()?.sync_no_fail();
                    maker.get_wallet().read()?.save_to_disk()?;
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
    let addr = maker.get_wallet().write()?.get_next_external_address()?;
    while !maker.shutdown.load(Relaxed) {
        maker.get_wallet().write()?.sync_no_fail();
        let offer_max_size = maker.get_wallet().read()?.store.offer_maxsize;

        let min_required = maker.config.min_swap_amount;
        if offer_max_size < min_required {
            log::warn!(
                "Low Swap Liquidity | Min: {} sats | Available: {} sats. Add funds to {:?}",
                min_required,
                offer_max_size,
                addr
            );

            sleep_duration = (sleep_duration + sleep_incremental).min(10 * 60); // Capped at 1 Block interval
            log::info!("Next sync in {:?} secs", sleep_duration);
            thread::sleep(Duration::from_secs(sleep_duration));
        } else {
            log::info!(
                "Swap Liquidity: {} sats | Min: {} sats | Listening for requests.",
                offer_max_size,
                min_required
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
        let mut taker_msg_bytes = Vec::new();
        match read_message(stream) {
            Ok(b) => taker_msg_bytes = b,
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

        let taker_msg: TakerToMakerMessage = serde_cbor::from_slice(&taker_msg_bytes)?;
        log::info!("[{}] <=== {}", maker.config.network_port, taker_msg);

        let reply = handle_message(maker, &mut connection_state, taker_msg);

        match reply {
            Ok(reply) => {
                if let Some(message) = reply {
                    log::info!("[{}] ===> {} ", maker.config.network_port, message);
                    if let Err(e) = send_message(stream, &message) {
                        log::error!("Closing due to IO error in sending message: {:?}", e);
                        continue;
                    }
                } else {
                    continue;
                }
            }
            Err(err) => {
                match &err {
                    // Shutdown server if special behavior is set
                    MakerError::SpecialBehaviour(sp) => {
                        log::error!(
                            "[{}] Maker Special Behavior : {:?}",
                            maker.config.network_port,
                            sp
                        );
                        maker.shutdown.store(true, Relaxed);
                    }
                    e => {
                        log::error!(
                            "[{}] Internal message handling error occurred: {:?}",
                            maker.config.network_port,
                            e
                        );
                    }
                }
                return Err(err);
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
    let (maker_addr, dns_addr) = network_bootstrap(maker.clone())?;

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
        // This threads check idelness of peer in live swaps.
        // And takes recovery measure if the peer seems to have disappeared in middlle of a swap.
        let maker_clone = maker.clone();
        let idle_conn_check_thread = thread::Builder::new()
            .name("Idle Client Checker Thread".to_string())
            .spawn(move || {
                log::info!(
                    "[{}] Spawning Client connection status checker thread",
                    network_port
                );
                if let Err(e) = check_for_idle_states(maker_clone.clone()) {
                    log::error!("Failed checking client's idle state {:?}", e);
                    maker_clone.shutdown.store(true, Relaxed);
                }
            })?;
        maker.thread_pool.add_thread(idle_conn_check_thread);

        // 2. Watchtower thread.
        // This thread checks for broadcasted contract transactions, which usually means violation of the protocol.
        // When contract transaction detected in mempool it will attempt recovery.
        // This can get triggered even when contracts of adjacent hops are published. Implying the whole swap route is disrupted.
        let maker_clone = maker.clone();
        let contract_watcher_thread = thread::Builder::new()
            .name("Contract Watcher Thread".to_string())
            .spawn(move || {
                log::info!("[{}] Spawning contract-watcher thread", network_port);
                if let Err(e) = check_for_broadcasted_contracts(maker_clone.clone()) {
                    maker_clone.shutdown.store(true, Relaxed);
                    log::error!("Failed checking broadcasted contracts {:?}", e);
                }
            })?;
        maker.thread_pool.add_thread(contract_watcher_thread);

        // 3: The RPC server thread.
        // User for responding back to `maker-cli` apps.
        let maker_clone = maker.clone();
        let rpc_thread = thread::Builder::new()
            .name("RPC Thread".to_string())
            .spawn(move || {
                log::info!("[{}] Spawning RPC server thread", network_port);
                match start_rpc_server(maker_clone.clone()) {
                    Ok(_) => (),
                    Err(e) => {
                        log::error!("Failed starting rpc server {:?}", e);
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
        if interval_tracker % RPC_PING_INTERVAL == 0 {
            check_connection_with_core(maker.as_ref())?;
        }

        // Perform fidelity bond and liquidity checks only when no coinswap is in progress.
        // This prevents the server from getting blocked while creating a new bond or waiting
        // for additional funds, which could otherwise interrupt an ongoing swap.
        // Running these checks during an active swap might cause the maker to stop responding,
        // potentially aborting the swap.
        if maker.ongoing_swap_state.lock()?.is_empty() {
            if interval_tracker % FIDELITY_BOND_DNS_UPDATE_INTERVAL == 0 {
                manage_fidelity_bonds_and_update_dns(maker.as_ref(), &maker_addr, &dns_addr)?;
                interval_tracker = 0;
            }

            if interval_tracker % SWAP_LIQUIDITY_CHECK_INTERVAL == 0 {
                check_swap_liquidity(maker.as_ref())?;
            }
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                log::info!("[{}] Received incoming connection", network_port);

                if let Err(e) = handle_client(&maker, &mut stream) {
                    log::error!("[{}] Error Handling client request {:?}", network_port, e);
                }
            }

            Err(e) => {
                if e.kind() != ErrorKind::WouldBlock {
                    log::error!(
                        "[{}] Error accepting incoming connection: {:?}",
                        network_port,
                        e
                    );
                }
            }
        };

        // Increment **interval_tracker** only if no coinswap is in progress or if no pending
        // swap liquidity and fidelity bond checks are due. This ensures these checks are
        // not skipped due to an ongoing coinswap and are performed once it completes.
        if maker.ongoing_swap_state.lock()?.is_empty()
            || interval_tracker % SWAP_LIQUIDITY_CHECK_INTERVAL != 0
            || interval_tracker % FIDELITY_BOND_DNS_UPDATE_INTERVAL != 0
        {
            interval_tracker += HEART_BEAT_INTERVAL.as_secs() as u32;
        }

        sleep(HEART_BEAT_INTERVAL);
    }

    log::info!("[{}] Maker is shutting down.", network_port);
    maker.thread_pool.join_all_threads()?;

    log::info!("Shutdown wallet sync initiated.");
    maker.get_wallet().write()?.sync_no_fail();
    log::info!("Shutdown wallet syncing completed.");
    maker.get_wallet().read()?.save_to_disk()?;
    log::info!("Wallet file saved to disk.");
    log::info!("Maker Server is shut down successfully");
    Ok(())
}

//! Unified Coinswap Maker Server.

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

use crate::{
    nostr_coinswap::broadcast_bond_on_nostr,
    protocol::{
        common_messages::FidelityProof,
        router::{MakerToTakerMessage, TakerToMakerMessage},
    },
};

use super::{
    error::MakerError,
    unified_api::UnifiedMakerServer,
    unified_handlers::{handle_message, UnifiedConnectionState},
};

/// Heartbeat interval for connections.
pub const HEART_BEAT_INTERVAL: Duration = Duration::from_secs(3);

/// Idle connection timeout (production).
#[cfg(not(feature = "integration-test"))]
pub const IDLE_CONNECTION_TIMEOUT: Duration = Duration::from_secs(900);

/// Idle connection timeout (testing).
#[cfg(feature = "integration-test")]
pub const IDLE_CONNECTION_TIMEOUT: Duration = Duration::from_secs(60);

/// Start a unified maker server.
pub fn start_unified_server(maker: Arc<UnifiedMakerServer>) -> Result<(), MakerError> {
    log::info!(
        "[{}] Starting unified maker server",
        maker.config.network_port
    );

    let maker_address = if cfg!(feature = "integration-test") {
        format!("127.0.0.1:{}", maker.config.network_port)
    } else {
        // In production, would use Tor hostname
        format!("127.0.0.1:{}", maker.config.network_port)
    };

    log::info!(
        "[{}] Setting up fidelity bond...",
        maker.config.network_port
    );
    let fidelity_proof = maker.setup_fidelity_bond(&maker_address)?;

    spawn_nostr_broadcast_thread(&maker, fidelity_proof)?;

    log::info!("[{}] Checking swap liquidity...", maker.config.network_port);
    maker.check_swap_liquidity()?;

    {
        let wallet = maker
            .wallet
            .read()
            .map_err(|_| MakerError::General("Failed to lock wallet"))?;
        log::info!(
            "[{}] Bitcoin Network: {}",
            maker.config.network_port,
            wallet.store.network
        );
        log::info!(
            "[{}] Spendable Wallet Balance: {}",
            maker.config.network_port,
            wallet.get_balances().map_err(MakerError::Wallet)?.spendable
        );
    }

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, maker.config.network_port))
        .map_err(MakerError::IO)?;
    listener.set_nonblocking(true).map_err(MakerError::IO)?;

    maker.is_setup_complete.store(true, Relaxed);
    log::info!(
        "[{}] Unified server setup complete! Listening on port {}",
        maker.config.network_port,
        maker.config.network_port
    );

    // Spawn idle state checker thread for recovery
    let maker_clone = Arc::clone(&maker);
    let idle_handle = thread::Builder::new()
        .name("unified-idle-checker".to_string())
        .spawn(move || {
            if let Err(e) = check_for_idle_states(maker_clone) {
                log::error!("Idle state checker error: {:?}", e);
            }
        })
        .map_err(MakerError::IO)?;
    maker.thread_pool.add_thread(idle_handle);

    while !maker.is_shutdown() {
        match listener.accept() {
            Ok((stream, addr)) => {
                log::info!(
                    "[{}] New connection from {}",
                    maker.config.network_port,
                    addr
                );

                let maker_clone = Arc::clone(&maker);
                thread::Builder::new()
                    .name(format!("connection-{}", addr))
                    .spawn(move || {
                        if let Err(e) = handle_connection(maker_clone, stream) {
                            log::error!("Connection error: {:?}", e);
                        }
                    })
                    .map_err(MakerError::IO)?;
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                // No connection waiting, sleep briefly
                sleep(Duration::from_millis(100));
            }
            Err(e) => {
                log::error!("[{}] Accept error: {}", maker.config.network_port, e);
            }
        }
    }

    log::info!(
        "[{}] Unified server shutting down...",
        maker.config.network_port
    );

    maker.watch_service.shutdown();
    maker.thread_pool.join_all_threads()?;

    log::info!(
        "[{}] Sync at:----Shutdown wallet----",
        maker.config.network_port
    );
    maker
        .wallet
        .write()
        .map_err(|_| MakerError::General("Failed to lock wallet"))?
        .sync_and_save()
        .map_err(MakerError::Wallet)?;

    log::info!("[{}] Server shutdown complete", maker.config.network_port);

    Ok(())
}

/// Spawn a background thread for nostr bond announcements.
fn spawn_nostr_broadcast_thread(
    maker: &Arc<UnifiedMakerServer>,
    fidelity: FidelityProof,
) -> Result<(), MakerError> {
    log::info!(
        "[{}] Spawning nostr background task",
        maker.config.network_port
    );

    let maker_clone = Arc::clone(maker);

    let handle = thread::Builder::new()
        .name("unified-nostr-thread".to_string())
        .spawn(move || {
            // Initial broadcast
            if let Err(e) = broadcast_bond_on_nostr(fidelity.clone()) {
                log::warn!("Initial nostr broadcast failed: {:?}", e);
            }

            let interval = Duration::from_secs(30 * 60); // 30 minutes
            let tick = Duration::from_secs(2);
            let mut elapsed = Duration::ZERO;

            while !maker_clone.shutdown.load(Ordering::Acquire) {
                thread::sleep(tick);
                elapsed += tick;

                if elapsed < interval {
                    continue;
                }

                elapsed = Duration::ZERO;

                log::debug!("Re-pinging nostr relays with bond announcement");

                if let Err(e) = broadcast_bond_on_nostr(fidelity.clone()) {
                    log::warn!("Nostr re-ping failed: {:?}", e);
                }
            }

            log::info!("Nostr background task stopped");
        })
        .map_err(MakerError::IO)?;

    maker.thread_pool.add_thread(handle);

    Ok(())
}

/// Handle a single connection.
fn handle_connection(maker: Arc<UnifiedMakerServer>, stream: TcpStream) -> Result<(), MakerError> {
    stream.set_nonblocking(false).map_err(MakerError::IO)?;
    stream
        .set_read_timeout(Some(IDLE_CONNECTION_TIMEOUT))
        .map_err(MakerError::IO)?;

    let mut state = UnifiedConnectionState::default();

    log::debug!(
        "[{}] Starting connection handler",
        maker.config.network_port
    );

    loop {
        // Check for shutdown
        if maker.is_shutdown() {
            log::info!(
                "[{}] Shutdown requested, closing connection",
                maker.config.network_port
            );
            break;
        }

        if state.is_timed_out(IDLE_CONNECTION_TIMEOUT.as_secs()) {
            log::info!("[{}] Connection timed out", maker.config.network_port);
            break;
        }

        let message = match read_unified_message(&stream) {
            Ok(msg) => msg,
            Err(e) => {
                log::debug!(
                    "[{}] Read error (may be normal disconnect): {:?}",
                    maker.config.network_port,
                    e
                );
                break;
            }
        };

        log::debug!(
            "[{}] Received message: {:?}",
            maker.config.network_port,
            message
        );

        let response = match handle_message(&maker, &mut state, message) {
            Ok(resp) => resp,
            Err(e) => {
                log::error!("[{}] Handler error: {:?}", maker.config.network_port, e);
                // Some errors are recoverable, some are not
                break;
            }
        };

        if let Some(response) = response {
            log::debug!(
                "[{}] Sending response: {:?}",
                maker.config.network_port,
                response
            );

            if let Err(e) = send_unified_message(&stream, &response) {
                log::error!(
                    "[{}] Failed to send response: {:?}",
                    maker.config.network_port,
                    e
                );
                break;
            }
        }

        if state.phase == super::unified_handlers::SwapPhase::Completed {
            log::info!(
                "[{}] Swap completed successfully",
                maker.config.network_port
            );
            break;
        }
    }

    log::debug!(
        "[{}] Connection handler finished",
        maker.config.network_port
    );

    Ok(())
}

/// Background thread that checks for idle swap states and spawns recovery.
fn check_for_idle_states(maker: Arc<UnifiedMakerServer>) -> Result<(), MakerError> {
    use super::swap_tracker::{now_secs, MakerRecoveryState, MakerSwapPhase, MakerSwapRecord};

    loop {
        if maker.is_shutdown() {
            break;
        }

        let idle_swaps = maker.drain_idle_swaps(IDLE_CONNECTION_TIMEOUT);

        for idle in idle_swaps {
            log::error!(
                "[{}] Potential dropped connection from taker. Swap {} idle. Recovering from swap",
                maker.config.network_port,
                idle.swap_id
            );

            // Create a tracker record for this dropped swap.
            let now = now_secs();
            let record = MakerSwapRecord {
                swap_id: idle.swap_id.clone(),
                protocol: idle.protocol,
                phase: MakerSwapPhase::TakerDropped,
                swap_amount_sat: idle.swap_amount_sat,
                incoming_count: idle.incoming_swapcoins.len(),
                outgoing_count: idle.outgoing_swapcoins.len(),
                funding_broadcast: idle.funding_broadcast,
                recovery: MakerRecoveryState::default(),
                created_at: now,
                updated_at: now,
            };

            if let Err(e) = maker.swap_tracker.lock().unwrap().save_record(&record) {
                log::error!("Failed to save swap tracker record: {:?}", e);
            }

            let swap_id = idle.swap_id.clone();
            let maker_clone = Arc::clone(&maker);
            let handle = thread::Builder::new()
                .name(format!("swap-recovery-{}", swap_id))
                .spawn(move || {
                    if let Err(e) = recover_from_swap(
                        maker_clone,
                        idle.swap_id,
                        idle.incoming_swapcoins,
                        idle.outgoing_swapcoins,
                    ) {
                        log::error!("Failed to recover from swap {}: {:?}", swap_id, e);
                    }
                })
                .map_err(MakerError::IO)?;
            maker.thread_pool.add_thread(handle);
        }

        sleep(HEART_BEAT_INTERVAL);
    }

    Ok(())
}

/// Minimum witness items for a hashlock spend (signature + preimage).
const MIN_WITNESS_ITEM_FOR_HASHLOCK: usize = 2;

/// Preimage length in bytes.
const PREIMAGE_LEN: usize = 32;

/// Check the watch tower for spends on outgoing contract outputs, extract
/// preimages from hashlock spends, and update incoming swapcoins in the wallet.
fn check_for_preimage_via_watchtower(
    maker: &UnifiedMakerServer,
    outgoing_swapcoins: &[crate::wallet::unified_swapcoin::OutgoingSwapCoin],
    incoming_swapcoins: &[crate::wallet::unified_swapcoin::IncomingSwapCoin],
) -> Result<(), MakerError> {
    use bitcoin::hashes::Hash;
    use std::{collections::HashSet, convert::TryFrom};

    let mut seen_outpoints = HashSet::new();
    let mut preimages: Vec<[u8; 32]> = Vec::new();

    // Query the watch tower for spends on each outgoing contract output.
    for outgoing in outgoing_swapcoins {
        let contract_txid = outgoing.contract_tx.compute_txid();
        for (vout, _) in outgoing.contract_tx.output.iter().enumerate() {
            let outpoint = bitcoin::OutPoint {
                txid: contract_txid,
                vout: vout as u32,
            };
            maker.watch_service.watch_request(outpoint);

            if let Some(crate::watch_tower::watcher::WatcherEvent::UtxoSpent {
                spending_tx: Some(spending_tx),
                ..
            }) = maker.watch_service.wait_for_event()
            {
                // Extract preimages from the spending transaction's witnesses.
                for input in &spending_tx.input {
                    let op = (input.previous_output.txid, input.previous_output.vout);
                    if seen_outpoints.insert(op)
                        && input.witness.len() >= MIN_WITNESS_ITEM_FOR_HASHLOCK
                        && input.witness[1].len() == PREIMAGE_LEN
                    {
                        if let Ok(preimage) = <[u8; 32]>::try_from(&input.witness[1][..]) {
                            preimages.push(preimage);
                        }
                    }
                }
            }
        }
    }

    if preimages.is_empty() {
        return Ok(());
    }

    log::info!(
        "[{}] Extracted {} preimage(s) from on-chain hashlock spends",
        maker.config.network_port,
        preimages.len()
    );

    // Apply extracted preimages to incoming swapcoins in the wallet.
    let mut wallet = maker
        .wallet
        .write()
        .map_err(|_| MakerError::General("Failed to lock wallet"))?;

    for incoming in incoming_swapcoins {
        // The wallet stores incoming swapcoins keyed by contract txid, not swap_id.
        let wallet_key = incoming.contract_tx.compute_txid().to_string();

        for preimage in &preimages {
            // Verify the preimage matches the incoming swapcoin's hashlock.
            let matches = if let Some(redeemscript) = incoming.contract_redeemscript() {
                // Legacy: uses OP_HASH160 with 20-byte hash
                let hash: bitcoin::hashes::hash160::Hash = bitcoin::hashes::Hash::hash(preimage);
                crate::protocol::contract::read_hashvalue_from_contract(redeemscript)
                    .map(|h| h == hash)
                    .unwrap_or(false)
            } else {
                // Taproot: uses OP_SHA256 with 32-byte hash
                // Script format: OP_SHA256 OP_PUSHBYTES_32 <32-byte hash> OP_EQUALVERIFY ...
                let sha256_hash: [u8; 32] =
                    bitcoin::hashes::sha256::Hash::hash(preimage).to_byte_array();
                incoming
                    .hashlock_script()
                    .map(|script| {
                        let bytes = script.as_bytes();
                        bytes.len() >= 34 && bytes[2..34] == sha256_hash
                    })
                    .unwrap_or(false)
            };

            if matches {
                if let Some(swapcoin) = wallet.find_unified_incoming_swapcoin_mut(&wallet_key) {
                    if swapcoin.hash_preimage.is_none() {
                        swapcoin.set_preimage(*preimage);
                        log::info!(
                            "[{}] Applied extracted preimage to incoming swapcoin {}",
                            maker.config.network_port,
                            wallet_key
                        );
                    }
                }
                break;
            }
        }
    }

    wallet.save_to_disk().map_err(MakerError::Wallet)?;

    Ok(())
}

/// Update the maker swap tracker with the given closure.
///
/// Locks the tracker, applies `f` to the record matching `swap_id`, then flushes.
fn update_tracker(
    maker: &UnifiedMakerServer,
    swap_id: &str,
    f: impl FnOnce(&mut super::swap_tracker::MakerSwapRecord),
) {
    let mut tracker = maker.swap_tracker.lock().unwrap();
    if let Some(record) = tracker.get_record_mut(swap_id) {
        f(record);
        record.updated_at = super::swap_tracker::now_secs();
        let cloned = record.clone();
        if let Err(e) = tracker.save_record(&cloned) {
            log::error!("Failed to flush swap tracker: {:?}", e);
        }
    }
}

/// Recover maker funds after taker drops.
///
/// Two recovery paths are tried in a loop:
/// 1. **Hashlock** (incoming swapcoins): If the taker (or another party) spends
///    our outgoing contract output via hashlock, the preimage is revealed on-chain.
///    We extract it via the watch tower and sweep our incoming swapcoins.
/// 2. **Timelock** (outgoing swapcoins): After the timelock expires, we reclaim
///    our outgoing funds via the timelock spending path.
fn recover_from_swap(
    maker: Arc<UnifiedMakerServer>,
    swap_id: String,
    incoming_swapcoins: Vec<crate::wallet::unified_swapcoin::IncomingSwapCoin>,
    outgoing_swapcoins: Vec<crate::wallet::unified_swapcoin::OutgoingSwapCoin>,
) -> Result<(), MakerError> {
    use super::swap_tracker::{MakerRecoveryPhase, MakerSwapPhase};
    use bitcoind::bitcoincore_rpc::RpcApi;

    // For Taproot, get_timelock() returns an absolute CLTV height.
    // For Legacy, it returns a relative CSV offset — but Legacy recovery
    // uses wallet-level methods that handle CSV internally, so we only
    // need the absolute value here for the monitoring loop.
    let timelock_expiry = outgoing_swapcoins
        .first()
        .and_then(|o| o.get_timelock())
        .ok_or(MakerError::General("missing timelock on outgoing swapcoin"))?;

    let start_height = maker
        .wallet
        .read()
        .map_err(|_| MakerError::General("Failed to lock wallet"))?
        .rpc
        .get_block_count()
        .map_err(crate::wallet::WalletError::Rpc)? as u32;

    log::info!(
        "[{}] recover_from_swap started | height={} timelock_expiry={} | incoming={} outgoing={}",
        maker.config.network_port,
        start_height,
        timelock_expiry,
        incoming_swapcoins.len(),
        outgoing_swapcoins.len()
    );

    // Check if funding was ever broadcast. If not, there is nothing on-chain
    // to recover — discard the swapcoins and exit immediately.
    {
        let funding_broadcast = maker
            .swap_tracker
            .lock()
            .unwrap()
            .get_record(&swap_id)
            .map(|r| r.funding_broadcast)
            .unwrap_or(false);

        if !funding_broadcast {
            log::info!(
                "[{}] Funding was never broadcast for swap {} — nothing to recover. Discarding swapcoins.",
                maker.config.network_port,
                swap_id
            );

            {
                let mut wallet = maker
                    .wallet
                    .write()
                    .map_err(|_| MakerError::General("Failed to lock wallet"))?;
                for outgoing in &outgoing_swapcoins {
                    let key = outgoing.contract_tx.compute_txid().to_string();
                    wallet.remove_unified_outgoing_swapcoin(&key);
                }
                for incoming in &incoming_swapcoins {
                    let key = incoming.contract_tx.compute_txid().to_string();
                    wallet.remove_unified_incoming_swapcoin(&key);
                }
                wallet.save_to_disk().map_err(MakerError::Wallet)?;
            }

            update_tracker(&maker, &swap_id, |r| {
                r.phase = MakerSwapPhase::Recovered;
                r.recovery.phase = MakerRecoveryPhase::CleanedUp;
            });

            #[cfg(feature = "integration-test")]
            maker.shutdown.store(true, Relaxed);
            return Ok(());
        }
    }

    // Tracker: Recovering + Monitoring
    update_tracker(&maker, &swap_id, |r| {
        r.phase = MakerSwapPhase::Recovering;
        r.recovery.phase = MakerRecoveryPhase::Monitoring;
    });

    // NOTE: Do NOT re-register outgoing contract outputs here.
    // They were already registered with the watch tower during swap setup
    // (in legacy_handlers / taproot_handlers). Re-registering would overwrite
    // the registry entry and lose any recorded `spent_tx` from on-chain
    // hashlock spends that the watcher already captured.

    while !maker.is_shutdown() {
        // --- Hashlock path: check if preimages are available ---
        check_for_preimage_via_watchtower(&maker, &outgoing_swapcoins, &incoming_swapcoins)?;

        // Check if all incoming swapcoins now have preimages
        let all_preimages_known = {
            let wallet = maker
                .wallet
                .read()
                .map_err(|_| MakerError::General("Failed to lock wallet"))?;
            incoming_swapcoins.iter().all(|incoming| {
                // Wallet stores incoming swapcoins keyed by contract txid.
                let key = incoming.contract_tx.compute_txid().to_string();
                wallet
                    .find_unified_incoming_swapcoin(&key)
                    .is_some_and(|s| s.is_preimage_known())
            })
        };

        if all_preimages_known && !incoming_swapcoins.is_empty() {
            log::info!(
                "[{}] All preimages known, recovering via hashlock path",
                maker.config.network_port
            );

            maker
                .wallet
                .write()
                .map_err(|_| MakerError::General("Failed to lock wallet"))?
                .sync_and_save()
                .map_err(MakerError::Wallet)?;

            let swept = maker
                .wallet
                .write()
                .map_err(|_| MakerError::General("Failed to lock wallet"))?
                .sweep_unified_incoming_swapcoins(crate::utill::MIN_FEE_RATE)
                .map_err(MakerError::Wallet)?;

            if !swept.is_empty() {
                log::info!(
                    "[{}] Recovered {} incoming swapcoins via hashlock",
                    maker.config.network_port,
                    swept.len()
                );

                // Tracker: HashlockRecovered
                update_tracker(&maker, &swap_id, |r| {
                    r.recovery.incoming_swept = swept.clone();
                    r.recovery.phase = MakerRecoveryPhase::HashlockRecovered;
                });

                // Clean up outgoing swapcoins — their funding was spent by
                // someone else (hashlock), so they are no longer recoverable
                // via timelock. Remove them from the wallet store.
                {
                    let mut wallet = maker
                        .wallet
                        .write()
                        .map_err(|_| MakerError::General("Failed to lock wallet"))?;
                    for outgoing in &outgoing_swapcoins {
                        // Wallet stores outgoing swapcoins keyed by contract txid.
                        let key = outgoing.contract_tx.compute_txid().to_string();
                        wallet.remove_unified_outgoing_swapcoin(&key);
                    }
                    wallet.save_to_disk().map_err(MakerError::Wallet)?;
                }

                // Tracker: Recovered + CleanedUp
                update_tracker(&maker, &swap_id, |r| {
                    r.phase = MakerSwapPhase::Recovered;
                    r.recovery.phase = MakerRecoveryPhase::CleanedUp;
                });

                #[cfg(feature = "integration-test")]
                maker.shutdown.store(true, Relaxed);
                return Ok(());
            }
        }

        // --- Timelock path: reclaim outgoing after timelock expires ---
        let current_height = maker
            .wallet
            .read()
            .map_err(|_| MakerError::General("Failed to lock wallet"))?
            .rpc
            .get_block_count()
            .map_err(crate::wallet::WalletError::Rpc)? as u32;

        if current_height >= timelock_expiry {
            log::info!(
                "[{}] Timelock expired at {} (expiry={}), recovering via timelock path",
                maker.config.network_port,
                current_height,
                timelock_expiry
            );

            // Tracker: TimelockWaiting
            update_tracker(&maker, &swap_id, |r| {
                r.recovery.phase = MakerRecoveryPhase::TimelockWaiting;
            });

            log::info!("Sync at:----recover_from_swap timelock----");
            maker
                .wallet
                .write()
                .map_err(|_| MakerError::General("Failed to lock wallet"))?
                .sync_and_save()
                .map_err(MakerError::Wallet)?;

            let recovered = maker
                .wallet
                .write()
                .map_err(|_| MakerError::General("Failed to lock wallet"))?
                .recover_unified_timelocked_swapcoins(crate::utill::MIN_FEE_RATE)
                .map_err(MakerError::Wallet)?;

            if !recovered.is_empty() {
                log::info!(
                    "[{}] Recovered {} outgoing swapcoins via timelock",
                    maker.config.network_port,
                    recovered.len()
                );

                // Tracker: TimelockRecovered → Recovered + CleanedUp
                update_tracker(&maker, &swap_id, |r| {
                    r.recovery.outgoing_recovered = recovered;
                    r.recovery.phase = MakerRecoveryPhase::TimelockRecovered;
                });
                update_tracker(&maker, &swap_id, |r| {
                    r.phase = MakerSwapPhase::Recovered;
                    r.recovery.phase = MakerRecoveryPhase::CleanedUp;
                });

                #[cfg(feature = "integration-test")]
                maker.shutdown.store(true, Relaxed);
                return Ok(());
            }
        }

        sleep(HEART_BEAT_INTERVAL);
    }

    Ok(())
}

/// Read a unified message from a stream.
fn read_unified_message(stream: &TcpStream) -> Result<TakerToMakerMessage, MakerError> {
    let mut len_buf = [0u8; 4];
    use std::io::Read;

    let mut stream_ref = stream;
    stream_ref
        .read_exact(&mut len_buf)
        .map_err(MakerError::IO)?;

    let len = u32::from_be_bytes(len_buf) as usize;

    if len > 10 * 1024 * 1024 {
        return Err(MakerError::General("Message too large"));
    }

    let mut buf = vec![0u8; len];
    stream_ref.read_exact(&mut buf).map_err(MakerError::IO)?;

    let message: TakerToMakerMessage = serde_cbor::from_slice(&buf)
        .map_err(|_| MakerError::General("Failed to deserialize message"))?;

    Ok(message)
}

/// Send a unified message to a stream.
fn send_unified_message(
    stream: &TcpStream,
    message: &MakerToTakerMessage,
) -> Result<(), MakerError> {
    let buf = serde_cbor::to_vec(message)
        .map_err(|_| MakerError::General("Failed to serialize message"))?;

    let len = buf.len() as u32;
    use std::io::Write;

    let mut stream_ref = stream;
    stream_ref
        .write_all(&len.to_be_bytes())
        .map_err(MakerError::IO)?;

    stream_ref.write_all(&buf).map_err(MakerError::IO)?;
    stream_ref.flush().map_err(MakerError::IO)?;

    Ok(())
}

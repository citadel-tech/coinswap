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

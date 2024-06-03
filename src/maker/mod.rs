//! Defines a Coinswap Maker Server.
//!
//! Handles connections, communication with takers, various aspects of the
//! Maker's behavior, includes functionalities such as checking for new connections,
//! handling messages from takers, refreshing offer caches, and interacting with the Bitcoin node.

pub mod api;
pub mod config;
pub mod error;
mod handlers;
pub mod rpc;

use std::{
    fs,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use bitcoin::{absolute::LockTime, Amount};
use bitcoind::bitcoincore_rpc::RpcApi;

use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncReadExt, BufReader},
    net::{tcp::ReadHalf, TcpListener, TcpStream},
    select,
    sync::mpsc,
    time::sleep,
};

pub use api::{Maker, MakerBehavior};

use std::io::Read;
use tokio::io::AsyncWriteExt;
use tokio_socks::tcp::Socks5Stream;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct OnionAddress {
    port: String,
    onion_addr: String,
}

use crate::{
    maker::{
        api::{check_for_broadcasted_contracts, check_for_idle_states, ConnectionState},
        handlers::handle_message,
        rpc::start_rpc_server_thread,
    },
    protocol::messages::{MakerHello, MakerToTakerMessage, TakerToMakerMessage},
    utill::{monitor_log_for_completion, send_message, ConnectionType},
    wallet::WalletError,
};

use crate::maker::error::MakerError;

/// Initializes and starts the Maker server, handling connections and various
/// aspects of the Maker's behavior.
#[tokio::main]
pub async fn start_maker_server(maker: Arc<Maker>) -> Result<(), MakerError> {
    let maker_port = maker.config.port;

    let mut handle = None;

    let mut maker_address = format!("127.0.0.1:{}", maker_port);

    match maker.config.connection_type {
        ConnectionType::CLEARNET => {
            let mut directory_address = maker.config.directory_server_clearnet_address.clone();
            if cfg!(feature = "integration-test") {
                directory_address = format!("127.0.0.1:{}", 8080);
            }
            loop {
                match TcpStream::connect(directory_address.clone()).await {
                    Ok(mut stream) => {
                        let request_line = format!("POST {}\n", maker_address);
                        if let Err(e) = stream.write_all(request_line.as_bytes()).await {
                            // Error sending the payload, log and retry after waiting
                            log::warn!(
                                "[{}] Failed to send maker address to directory, reattempting: {}",
                                maker_port,
                                e
                            );
                            thread::sleep(Duration::from_secs(
                                maker.config.heart_beat_interval_secs,
                            ));
                            continue;
                        }
                        // Payload sent successfully, exit the loop
                        log::info!(
                            "[{}] Successfully sent maker address to directory",
                            maker_port
                        );
                        break;
                    }
                    Err(e) => {
                        // Connection error, log and retry after waiting
                        log::warn!(
                            "[{}] TCP connection error with directory, reattempting: {}",
                            maker_port,
                            e
                        );
                        thread::sleep(Duration::from_secs(maker.config.heart_beat_interval_secs));
                        continue;
                    }
                }
            }
        }
        ConnectionType::TOR => {
            if cfg!(feature = "tor") {
                let maker_socks_port = maker.config.socks_port;

                let tor_log_dir = format!("/tmp/tor-rust-maker{}/log", maker_port);

                if Path::new(tor_log_dir.as_str()).exists() {
                    match fs::remove_file(Path::new(tor_log_dir.as_str())) {
                        Ok(_) => log::info!(
                            "[{}] Previous Maker log file deleted successfully",
                            maker_port
                        ),
                        Err(_) => log::error!("[{}] Error deleting Maker log file", maker_port),
                    }
                }

                handle = Some(crate::tor::spawn_tor(
                    maker_socks_port,
                    maker_port,
                    format!("/tmp/tor-rust-maker{}", maker_port),
                ));
                thread::sleep(Duration::from_secs(10));

                if let Err(e) = monitor_log_for_completion(&PathBuf::from(tor_log_dir), "100%") {
                    log::error!("[{}] Error monitoring log file: {}", maker_port, e);
                }

                log::info!("Maker tor is instantiated");

                let maker_hs_path_str =
                    format!("/tmp/tor-rust-maker{}/hs-dir/hostname", maker.config.port);
                let maker_hs_path = PathBuf::from(maker_hs_path_str);
                let mut maker_file = fs::File::open(&maker_hs_path).unwrap();
                let mut maker_onion_addr: String = String::new();
                maker_file.read_to_string(&mut maker_onion_addr).unwrap();
                maker_onion_addr.pop();
                maker_address = format!("{}:{}", maker_onion_addr, maker.config.port);

                let mut directory_onion_address =
                    maker.config.directory_server_onion_address.clone();

                if cfg!(feature = "integration-test") {
                    let directory_hs_path_str =
                        "/tmp/tor-rust-directory/hs-dir/hostname".to_string();
                    let directory_hs_path = PathBuf::from(directory_hs_path_str);
                    let mut directory_file = fs::File::open(directory_hs_path).unwrap();
                    let mut directory_onion_addr: String = String::new();
                    directory_file
                        .read_to_string(&mut directory_onion_addr)
                        .unwrap();
                    directory_onion_addr.pop();
                    directory_onion_address = format!("{}:{}", directory_onion_addr, 8080);
                }

                let address = directory_onion_address.as_str();

                log::info!(
                    "[{}] Directory onion address : {}",
                    maker_port,
                    directory_onion_address
                );

                loop {
                    match Socks5Stream::connect(
                        format!("127.0.0.1:{}", maker_socks_port).as_str(),
                        address,
                    )
                    .await
                    {
                        Ok(socks_stream) => {
                            let mut stream = socks_stream.into_inner();
                            let request_line = format!("POST {}\n", maker_address);
                            if let Err(e) = stream.write_all(request_line.as_bytes()).await {
                                log::warn!(
                                    "[{}] Failed to send maker address to directory, reattempting: {}",
                                    maker_port,
                                    e
                                );
                                thread::sleep(Duration::from_secs(
                                    maker.config.heart_beat_interval_secs,
                                ));
                                continue;
                            }
                            log::info!(
                                "[{}] Sucessfuly sent maker address to directory",
                                maker_port
                            );
                            break;
                        }
                        Err(e) => {
                            log::warn!(
                                "[{}] Socks connection error with directory, reattempting: {}",
                                maker_port,
                                e
                            );
                            thread::sleep(Duration::from_secs(
                                maker.config.heart_beat_interval_secs,
                            ));
                            continue;
                        }
                    }
                }
            }
        }
    }

    maker.wallet.write()?.refresh_offer_maxsize_cache()?;

    let network = maker.get_wallet().read()?.store.network;
    log::info!("Network: {:?}", network);

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, maker.config.port)).await?;
    log::info!("Listening On Port {}", maker.config.port);

    let (server_loop_comms_tx, mut server_loop_comms_rx) = mpsc::channel::<MakerError>(100);
    let mut accepting_clients = true;
    let mut last_rpc_ping = Instant::now();
    // let mut last_directory_servers_refresh = Instant::now();

    let maker_clone_1 = maker.clone();
    std::thread::spawn(move || {
        log::info!(
            "[{}] Spawning Connection status check thread",
            maker_clone_1.config.port
        );
        check_for_idle_states(maker_clone_1).unwrap();
    });

    let maker_clone_2 = maker.clone();
    std::thread::spawn(move || {
        log::info!(
            "[{}] Spawning contract-watcher thread",
            maker_clone_2.config.port
        );
        check_for_broadcasted_contracts(maker_clone_2).unwrap();
    });

    // Get the highest value fidelity bond from the wallet.
    {
        let mut wallet = maker.wallet.write()?;
        if let Some(i) = wallet.get_highest_fidelity_index()? {
            let highest_proof = wallet.generate_fidelity_proof(i, maker.config.port.to_string())?;
            let mut proof = maker.highest_fidelity_proof.write()?;
            *proof = Some(highest_proof);
        } else {
            // No bond in the wallet. Lets attempt to create one.
            let amount = Amount::from_sat(maker.config.fidelity_value);
            let current_height = wallet.rpc.get_block_count().map_err(WalletError::Rpc)? as u32;

            // Set 100 blocks locktime for test
            let locktime = if cfg!(feature = "integration-test") {
                LockTime::from_height(current_height + 100).unwrap()
            } else {
                LockTime::from_height(maker.config.fidelity_timelock + current_height).unwrap()
            };

            match wallet.create_fidelity(amount, locktime) {
                // Hard error if we cant create fidelity. As without this Maker can't send a valid
                // Offer to taker.
                Err(e) => {
                    log::error!(
                        "[{}] Fidelity Bond Creation failed: {:?}. Shutting Down Maker server",
                        maker.config.port,
                        e
                    );
                    return Err(e.into());
                }
                Ok(i) => {
                    log::info!("[{}] Successfully created fidelity bond", maker.config.port);

                    let address_string = maker_address.clone();
                    let highest_proof = wallet.generate_fidelity_proof(i, address_string)?;
                    let mut proof = maker.highest_fidelity_proof.write()?;
                    *proof = Some(highest_proof);
                }
            }
        }
        log::info!("[{}] Syncing and saving wallet data", maker.config.port);
        wallet.sync()?;
        wallet.save_to_disk()?;
        log::info!("[{}] Sync and save successful", maker.config.port);
    }

    // Spawn the RPC Thread here.
    let rpc_maker = maker.clone();
    let _ = start_rpc_server_thread(rpc_maker).await;

    maker.setup_complete()?;

    log::info!("[{}] Maker setup is ready", maker.config.port);

    // Loop to keep checking for new connections
    let result = loop {
        if *maker.shutdown.read()? {
            log::warn!("[{}] Maker is shutting down", maker.config.port);
            break Ok(());
        }
        let (mut socket, addr) = select! {

            new_client = listener.accept() => new_client?,
            client_err = server_loop_comms_rx.recv() => {
                //unwrap the option here because we'll never close the mscp so it will always work
                match client_err.as_ref().unwrap() {
                    MakerError::Wallet(WalletError::Rpc(e)) => {
                        //doublecheck the rpc connection here because sometimes the rpc error
                        //will be unrelated to the connection itmaker e.g. "insufficent funds"
                        let rpc_connection_success = maker.wallet.read()?.rpc.get_best_block_hash().is_ok();
                        if !rpc_connection_success {
                            log::warn!("lost connection with bitcoin node, temporarily shutting \
                                        down server until connection reestablished, error={:?}", e);
                            accepting_clients = false;
                        }
                        continue;
                    },
                    _ => {
                        log::error!("[{}] Maker Handling Error : {:?}", maker.config.port, client_err.unwrap());
                        // Either in special Maker behavior, or something went worng.
                        // Quitely shutdown.
                        // TODO: Handle this behavior separately for prod/test.
                        maker.shutdown()?;
                        // We continue, as the shutdown flag will be caught in the next iteration of the loop.
                        // In the case below.
                        // Shutting down tor here
                        continue;
                    }
                }
            },
            _ = sleep(Duration::from_secs(maker.config.heart_beat_interval_secs)) => {
                let mut rpc_ping_success = true;

                let rpc_ping_interval = Duration::from_secs(maker.config.rpc_ping_interval_secs);
                if Instant::now().saturating_duration_since(last_rpc_ping) > rpc_ping_interval {
                    last_rpc_ping = Instant::now();
                    rpc_ping_success = maker.wallet.write()?.refresh_offer_maxsize_cache().is_ok();
                }
                accepting_clients = rpc_ping_success;
                if !accepting_clients {
                    log::warn!("not accepting clients, rpc_ping_success={}", rpc_ping_success);
                }
                continue;
            },
        };

        if !accepting_clients {
            log::warn!("Rejecting Connection From {:?}", addr);
            continue;
        }

        log::info!(
            "[{}] <=== Accepted Connection on port={}",
            maker.config.port,
            addr.port()
        );
        let server_loop_comms_tx = server_loop_comms_tx.clone();
        let maker_clone = maker.clone();

        // Spawn a thread to handle one taker connection.
        tokio::spawn(async move {
            log::info!("[{}] Spawning Handler Thread", maker_clone.config.port);
            let (socket_reader, mut socket_writer) = socket.split();
            let mut reader = BufReader::new(socket_reader);

            let mut connection_state = ConnectionState::default();

            if let Err(e) = send_message(
                &mut socket_writer,
                &MakerToTakerMessage::MakerHello(MakerHello {
                    protocol_version_min: 0,
                    protocol_version_max: 0,
                }),
            )
            .await
            {
                log::error!("IO error sending first message: {:?}", e);
                return;
            }
            log::info!("[{}] ===> MakerHello", maker_clone.config.port);

            loop {
                let message = select! {
                    read_result = read_taker_message(&mut reader) => {
                        match read_result {
                            Ok(None) => {
                                log::info!("[{}] Connection closed by peer", maker_clone.config.port);
                                break;
                            },
                            Ok(Some(msg)) => msg,
                            Err(e) => {
                                log::error!("error reading from socket: {:?}", e);
                                break;
                            }
                        }
                    },
                    _ = sleep(Duration::from_secs(maker_clone.config.idle_connection_timeout)) => {
                        log::info!("[{}] Idle connection closed", addr.port());
                        break;
                    },
                };

                log::info!("[{}] <=== {} ", maker_clone.config.port, message);

                let reply: Result<Option<MakerToTakerMessage>, MakerError> =
                    handle_message(&maker_clone, &mut connection_state, message, addr.ip()).await;

                match reply {
                    Ok(reply) => {
                        if let Some(message) = reply {
                            log::info!("[{}] ===> {} ", maker_clone.config.port, message);
                            if let Err(e) = send_message(&mut socket_writer, &message).await {
                                log::error!("Closing due to IO error in sending message: {:?}", e);
                                continue;
                            }
                        }
                        // if reply is None then don't send anything to client
                    }
                    Err(err) => {
                        server_loop_comms_tx.send(err).await.unwrap();
                        break;
                    }
                }
            }
        });
    };

    if maker.config.connection_type == ConnectionType::TOR && cfg!(feature = "tor") {
        crate::tor::kill_tor_handles(handle.unwrap());
    }

    result
}

/// Reads a Taker Message.
async fn read_taker_message(
    reader: &mut BufReader<ReadHalf<'_>>,
) -> Result<Option<TakerToMakerMessage>, MakerError> {
    let read_result = reader.read_u32().await;
    // If its EOF, return None
    if read_result
        .as_ref()
        .is_err_and(|e| e.kind() == std::io::ErrorKind::UnexpectedEof)
    {
        return Ok(None);
    }
    let length = read_result?;
    if length == 0 {
        return Ok(None);
    }
    let mut buffer = vec![0; length as usize];
    reader.read_exact(&mut buffer).await?;
    let message: TakerToMakerMessage = serde_cbor::from_slice(&buffer)?;
    Ok(Some(message))
}

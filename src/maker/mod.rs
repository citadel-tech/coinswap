//! Defines a Coinswap Maker Server.
//!
//! Handles connections, communication with takers, various aspects of the
//! Maker's behavior, includes functionalities such as checking for new connections,
//! handling messages from takers, refreshing offer caches, and interacting with the Bitcoin node.

pub mod api;
pub mod config;
pub mod error;
mod handlers;

use std::{
    net::Ipv4Addr, sync::Arc, time::{Duration, Instant}
};

use bitcoin::{absolute::LockTime, Amount, Network};
use bitcoind::bitcoincore_rpc::RpcApi;
use serde::{Serialize,Deserialize};
use tokio::{
    io::{AsyncReadExt,BufReader}, net::{tcp::ReadHalf, TcpListener}, select, sync::mpsc,time::sleep
};

pub use api::{Maker, MakerBehavior};

#[derive(Clone,Debug,Serialize,Deserialize)]
struct OnionAddress {
    port: String,
    onion_addr: String
}

use crate::{
    maker::{
        api::{check_for_broadcasted_contracts, check_for_idle_states, ConnectionState},
        handlers::handle_message,
    },
    market::directory::post_maker_address_to_directory_servers,
    protocol::messages::{MakerHello, MakerToTakerMessage, TakerToMakerMessage},
    utill::send_message,
    wallet::WalletError,
};

use crate::maker::error::MakerError;



/// Initializes and starts the Maker server, handling connections and various
/// aspects of the Maker's behavior.
#[tokio::main]
pub async fn start_maker_server(maker: Arc<Maker>) -> Result<(), MakerError> {
   
   
    log::debug!("Running maker with special behavior = {:?}", maker.behavior);
    maker.wallet.write()?.refresh_offer_maxsize_cache()?;

    let network = maker.get_wallet().read()?.store.network;
    log::info!("Network: {:?}", network);

    // let onion_addr = maker.config.onion_addrs.clone();

    if maker.wallet.read()?.store.network != Network::Regtest {
        if maker.config.onion_addrs == "myhiddenserviceaddress.onion:6102" {
            panic!("You must set config variable MAKER_ONION_ADDR in file src/maker_protocol.rs");
        }
        log::info!(
            "Adding my address ({}) to the directory servers. . .",
            maker.config.onion_addrs
        );
        post_maker_address_to_directory_servers(network, &maker.config.onion_addrs)
            .await
            .expect("unable to add my address to the directory servers, is tor reachable?");
    }

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
                    // FIXME: Hack to get the tests running. This should be the actual onion address.
                    let onion_string = "localhost:".to_string() + &maker.config.port.to_string();
                    let highest_proof = wallet.generate_fidelity_proof(i, onion_string)?;
                    let mut proof = maker.highest_fidelity_proof.write()?;
                    *proof = Some(highest_proof);
                }
            }
        }
    }

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, maker.config.port)).await?;
    log::info!("Listening On Port {}", maker.config.port);

    let (server_loop_comms_tx, mut server_loop_comms_rx) = mpsc::channel::<MakerError>(100);
    let mut accepting_clients = true;
    let mut last_rpc_ping = Instant::now();
    let mut last_directory_servers_refresh = Instant::now();

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

    // Loop to keep checking for new connections
    loop {
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
                    log::debug!("rpc_ping_success = {}", rpc_ping_success);
                }
                accepting_clients = rpc_ping_success;
                if !accepting_clients {
                    log::warn!("not accepting clients, rpc_ping_success={}", rpc_ping_success);
                }

                let directory_servers_refresh_interval = Duration::from_secs(
                    maker.config.directory_servers_refresh_interval_secs
                );
                let network = maker.get_wallet().read()?.store.network;
                if maker.wallet.read()?.store.network != Network::Regtest
                        && Instant::now().saturating_duration_since(last_directory_servers_refresh)
                        > directory_servers_refresh_interval {
                    last_directory_servers_refresh = Instant::now();
                    let result_expiry_time = post_maker_address_to_directory_servers(
                        network,
                        &maker.config.onion_addrs
                    ).await;
                    log::info!("Refreshing my address at the directory servers = {:?}",
                        result_expiry_time);
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
                            log::debug!("{:#?}", message);
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
    }
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

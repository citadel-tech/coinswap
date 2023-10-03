pub mod config;
pub mod error;
mod handlers;
pub mod maker;
//mod server;

use std::{
    net::Ipv4Addr,
    sync::Arc,
    time::{Duration, Instant},
};

use bitcoin::Network;
use bitcoind::bitcoincore_rpc::RpcApi;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    net::TcpListener,
    select,
    sync::mpsc,
    time::sleep,
};

pub use maker::{Maker, MakerBehavior};

use crate::{
    maker::{
        handlers::handle_message,
        maker::{check_for_broadcasted_contracts, check_for_idle_states, ConnectionState},
    },
    market::directory::post_maker_address_to_directory_servers,
    protocol::messages::{MakerHello, MakerToTakerMessage, TakerToMakerMessage},
    utill::send_message,
    wallet::WalletError,
};

use crate::maker::error::MakerError;

/// Start the Maker server loop
#[tokio::main]
pub async fn start_maker_server(maker: Arc<Maker>) -> Result<(), MakerError> {
    log::debug!("Running maker with special behavior = {:?}", maker.behavior);
    maker.wallet.write()?.refresh_offer_maxsize_cache()?;

    if maker.wallet.read()?.store.network != Network::Regtest {
        if maker.config.onion_addrs == "myhiddenserviceaddress.onion:6102" {
            panic!("You must set config variable MAKER_ONION_ADDR in file src/maker_protocol.rs");
        }
        log::info!(
            "Adding my address ({}) to the directory servers. . .",
            maker.config.onion_addrs
        );
        post_maker_address_to_directory_servers(
            maker.wallet.read()?.store.network,
            &maker.config.onion_addrs,
        )
        .await
        .expect("unable to add my address to the directory servers, is tor reachable?");
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
                if *maker.shutdown.read()? {
                    log::warn!("[{}] Maker is shutting down", maker.config.port);
                    break Ok(());
                }
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
                if maker.wallet.read()?.store.network != Network::Regtest
                        && Instant::now().saturating_duration_since(last_directory_servers_refresh)
                        > directory_servers_refresh_interval {
                    last_directory_servers_refresh = Instant::now();
                    let result_expiry_time = post_maker_address_to_directory_servers(
                        maker.wallet.read()?.store.network,
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
                log::error!("io error sending first message: {:?}", e);
                return;
            }
            log::info!("[{}] ===> MakerHello", maker_clone.config.port);

            loop {
                let mut line = String::new();
                select! {
                    readline_ret = reader.read_line(&mut line) => {
                        match readline_ret {
                            Ok(n) if n == 0 => {
                                log::info!("[{}] Connection closed by peer", maker_clone.config.port);
                                break;
                            }
                            Ok(_n) => (),
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

                line = line.trim_end().to_string();
                let message: TakerToMakerMessage = serde_json::from_str(&line).unwrap();
                log::info!("[{}] <=== {} ", maker_clone.config.port, message);

                let message_result: Result<Option<MakerToTakerMessage>, MakerError> =
                    handle_message(&maker_clone, &mut connection_state, message, addr.ip()).await;

                match message_result {
                    Ok(reply) => {
                        if let Some(message) = reply {
                            log::info!("[{}] ===> {} ", maker_clone.config.port, message);
                            log::debug!("{:#?}", message);
                            if let Err(e) = send_message(&mut socket_writer, &message).await {
                                log::error!("closing due to io error sending message: {:?}", e);
                                break;
                            }
                        }
                        //if reply is None then dont send anything to client
                    }
                    Err(err) => {
                        server_loop_comms_tx.send(err).await.unwrap();
                        break;
                    }
                };
            }
        });
    }
}

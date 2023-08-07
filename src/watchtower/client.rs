const WATCHTOWER_HOSTPORT: &str = "localhost:6103";

use std::time::Duration;

use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
    select,
    time::sleep,
};

use crate::error::TeleportError;

use super::routines::{
    ContractsInfo, MakerToWatchtowerMessage, Ping, WatchContractTxes, WatchtowerToMakerMessage,
};

pub const CONNECT_ATTEMPTS: u32 = 10;
pub const CONNECT_SLEEP_DELAY_SEC: u64 = 5;
pub const CONNECT_ATTEMPT_TIMEOUT_SEC: u64 = 10;

#[tokio::main]
pub async fn test_watchtower_client(contracts_to_watch: ContractsInfo) {
    ping_watchtowers().await.unwrap();
    register_coinswap_with_watchtowers(contracts_to_watch)
        .await
        .unwrap();
}

fn parse_message(line: &str) -> Result<WatchtowerToMakerMessage, TeleportError> {
    serde_json::from_str::<WatchtowerToMakerMessage>(line)
        .map_err(|_| TeleportError::Protocol("watchtower sent invalid message"))
}

pub async fn register_coinswap_with_watchtowers(
    contracts_to_watch: ContractsInfo,
) -> Result<(), TeleportError> {
    send_message_to_watchtowers(&MakerToWatchtowerMessage::WatchContractTxes(
        WatchContractTxes {
            protocol_version_min: 0,
            protocol_version_max: 0,
            contracts_to_watch,
        },
    ))
    .await?;
    log::info!("Successfully registered contract txes with watchtower");
    Ok(())
}

pub async fn ping_watchtowers() -> Result<(), TeleportError> {
    log::debug!("pinging watchtowers");
    send_message_to_watchtowers(&MakerToWatchtowerMessage::Ping(Ping {
        protocol_version_min: 0,
        protocol_version_max: 0,
    }))
    .await
}

async fn send_message_to_watchtowers_once(
    message: &MakerToWatchtowerMessage,
) -> Result<(), TeleportError> {
    //TODO add support for registering with multiple watchtowers concurrently

    let mut socket = TcpStream::connect(WATCHTOWER_HOSTPORT).await?;

    let (socket_reader, mut socket_writer) = socket.split();
    let mut socket_reader = BufReader::new(socket_reader);

    let mut message_packet = serde_json::to_vec(message).unwrap();
    message_packet.push(b'\n');
    socket_writer.write_all(&message_packet).await?;

    let mut line1 = String::new();
    if socket_reader.read_line(&mut line1).await? == 0 {
        return Err(TeleportError::Protocol("watchtower eof"));
    }
    let _watchtower_hello =
        if let WatchtowerToMakerMessage::WatchtowerHello(h) = parse_message(&line1)? {
            h
        } else {
            log::trace!(target: "watchtower_client", "wrong protocol message");
            return Err(TeleportError::Protocol(
                "wrong protocol message from watchtower",
            ));
        };
    log::trace!(target: "watchtower_client", "watchtower hello = {:?}", _watchtower_hello);

    let mut line2 = String::new();
    if socket_reader.read_line(&mut line2).await? == 0 {
        return Err(TeleportError::Protocol("watchtower eof"));
    }
    let _success = if let WatchtowerToMakerMessage::Success(s) = parse_message(&line2)? {
        s
    } else {
        log::trace!(target: "watchtower_client", "wrong protocol message2");
        return Err(TeleportError::Protocol(
            "wrong protocol message2 from watchtower",
        ));
    };

    Ok(())
}

async fn send_message_to_watchtowers(
    message: &MakerToWatchtowerMessage,
) -> Result<(), TeleportError> {
    let mut ii = 0;
    loop {
        ii += 1;
        select! {
            ret = send_message_to_watchtowers_once(message) => {
                match ret {
                    Ok(_) => return Ok(()),
                    Err(e) => {
                        log::warn!(
                            "Failed to send message to watchtower, reattempting... error={:?}",
                            e
                        );
                        if ii <= CONNECT_ATTEMPTS {
                            sleep(Duration::from_secs(CONNECT_SLEEP_DELAY_SEC)).await;
                            continue;
                        } else {
                            return Err(e);
                        }
                    }
                }
            },
            _ = sleep(Duration::from_secs(CONNECT_ATTEMPT_TIMEOUT_SEC)) => {
                log::warn!(
                    "Timeout for sending message to watchtower, reattempting...",
                );
                if ii <= CONNECT_ATTEMPTS {
                    continue;
                } else {
                    return Err(TeleportError::Protocol(
                        "Timed out of sending message to watchtower"));
                }
            },
        }
    }
}

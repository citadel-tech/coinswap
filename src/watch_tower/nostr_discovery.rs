//! Nostr discovery module.
//!
//! Handles the discovery of Maker fidelity bonds via Nostr relays. It creates persistent
//! subscriptions to CoinSwap-related events (kind 37777), validates incoming fidelity
//! announcements against the Bitcoin blockchain, and stores verified bonds in the registry.

use std::{
    borrow::Cow,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use nostr::{
    event::Kind,
    filter::Filter,
    message::{ClientMessage, RelayMessage, SubscriptionId},
    util::JsonUtil,
};
use tungstenite::{stream::MaybeTlsStream, Message};

use crate::{
    nostr_coinswap::{COINSWAP_KIND, NOSTR_RELAYS},
    watch_tower::{
        registry_storage::FileRegistry,
        rest_backend::BitcoinRest,
        utils::{parse_fidelity_event, process_fidelity, SeenTxids},
        watcher_error::WatcherError,
    },
};

// ## TODO: Instead of looping over relay's have a connection Pool.
/// Runs the main discovery routine for maker's fidelity bonds by subscribing to Nostr events (kind 37777).
pub fn run_discovery(
    bitcoin_rpc: BitcoinRest,
    registry: FileRegistry,
    shutdown: Arc<AtomicBool>,
) -> Result<(), WatcherError> {
    log::info!("Starting market discovery via Nostr");

    let seen_txid = Arc::new(Mutex::new(SeenTxids::new()));
    let registry = Arc::new(registry);
    let bitcoin_rpc = Arc::new(bitcoin_rpc);

    for relay in NOSTR_RELAYS {
        let relay = relay.to_string();
        let shutdown = shutdown.clone();
        let registry = Arc::clone(&registry);
        let bitcoin_rpc = Arc::clone(&bitcoin_rpc);
        let seen_txid = Arc::clone(&seen_txid);

        std::thread::Builder::new()
            .name(format!("nostr-session-{}", relay))
            .spawn(move || {
                run_nostr_session_for_relay(
                    &relay.clone(),
                    registry,
                    shutdown,
                    bitcoin_rpc,
                    &seen_txid,
                );
            })?;
    }

    Ok(())
}

/// Runs a long-lived Nostr session for a single relay.
/// Reconnects automatically until shutdown is requested.
fn run_nostr_session_for_relay(
    relay_url: &str,
    registry: Arc<FileRegistry>,
    shutdown: Arc<AtomicBool>,
    bitcoin_rpc: Arc<BitcoinRest>,
    seen_txid: &Arc<Mutex<SeenTxids>>,
) {
    log::info!("Starting Nostr session for relay {}", relay_url);

    while !shutdown.load(Ordering::SeqCst) {
        match connect_and_run_once(
            relay_url,
            registry.clone(),
            shutdown.clone(),
            bitcoin_rpc.clone(),
            seen_txid,
        ) {
            Ok(()) => {
                // Likely exited due to shutdown
                break;
            }
            Err(e) => {
                log::warn!(
                    "Nostr session error on {}: {:?}, retrying in 5s",
                    relay_url,
                    e
                );
                std::thread::sleep(std::time::Duration::from_secs(5));
            }
        }
    }

    log::info!("Stopped Nostr session for relay {}", relay_url);
}

/// Establishes websocket connection to single Nostr relay and processes events until error or shutdown.
/// Subscribe to Nostr events on Kind (37777).
fn connect_and_run_once(
    relay_url: &str,
    registry: Arc<FileRegistry>,
    shutdown: Arc<AtomicBool>,
    bitcoin_rpc: Arc<BitcoinRest>,
    seen_txid: &Arc<Mutex<SeenTxids>>,
) -> Result<(), WatcherError> {
    let (mut socket, _) = tungstenite::connect(relay_url)?;

    let filter = Filter::new().kind(Kind::Custom(COINSWAP_KIND));
    let req = ClientMessage::Req {
        subscription_id: Cow::Owned(SubscriptionId::new(format!(
            "market-discovery-{}",
            relay_url
        ))),
        filters: vec![Cow::Owned(filter)],
    };

    socket.write(Message::Text(req.as_json().into()))?;

    socket.flush()?;

    log::info!(
        "Subscribed to fidelity announcements on {} (kind={})",
        relay_url,
        COINSWAP_KIND
    );

    read_event_loop(
        registry,
        socket,
        shutdown,
        bitcoin_rpc,
        relay_url,
        seen_txid,
    )
}

/// Stream all the events from the Nostr relay and deserialize from json until shutdown
fn read_event_loop(
    registry: Arc<FileRegistry>,
    mut socket: tungstenite::WebSocket<MaybeTlsStream<std::net::TcpStream>>,
    shutdown: Arc<AtomicBool>,
    bitcoin_rpc: Arc<BitcoinRest>,
    relay_url: &str,
    seen_txid: &Arc<Mutex<SeenTxids>>,
) -> Result<(), WatcherError> {
    while !shutdown.load(Ordering::SeqCst) {
        let msg = socket.read()?;

        let text = match msg {
            Message::Text(t) => t,
            Message::Binary(b) => String::from_utf8(b.to_vec())?.into(),
            _ => continue,
        };

        let relay_msg = RelayMessage::from_json(&text)?;

        handle_relay_message(
            registry.clone(),
            relay_msg,
            bitcoin_rpc.clone(),
            relay_url,
            seen_txid,
        )?;
    }

    Ok(())
}

/// filter events based on kind and tags
/// check if event was alredy recived using the cache
/// Returns the fidelity announcement containing onion address
fn handle_relay_message(
    registry: Arc<FileRegistry>,
    msg: RelayMessage,
    bitcoin_rpc: Arc<BitcoinRest>,
    relay_url: &str,
    seen_txid: &Arc<Mutex<SeenTxids>>,
) -> Result<(), WatcherError> {
    match msg {
        RelayMessage::Event { event, .. } => {
            if event.kind != Kind::Custom(COINSWAP_KIND) {
                return Ok(());
            }

            if event.is_expired() || event.tags.expiration().is_none() {
                log::debug!(
                    "Ignoring expired event or event without expiration tag from {}",
                    relay_url
                );
                return Ok(());
            }

            let Some((txid, vout)) = parse_fidelity_event(&event) else {
                return Ok(());
            };

            if seen_txid.lock()?.insert(txid) {
                log::debug!("add new cache {}", txid);
                let Ok(tx) = bitcoin_rpc.get_raw_tx(&txid) else {
                    log::debug!("Received invalid txid: {txid:?}");
                    return Ok(());
                };

                match process_fidelity(&tx) {
                    Some(fidelity) => {
                        if registry.insert_fidelity(txid, fidelity) {
                            log::info!("Stored verified fidelity via {relay_url}: {txid}:{vout}");
                        }
                    }
                    None => {
                        log::debug!("Invalid fidelity {txid}:{vout} via {relay_url}");
                    }
                }
            } else {
                log::debug!("Transaction ID already present {txid} via {relay_url}")
            }
        }

        RelayMessage::EndOfStoredEvents(sub_id) => {
            log::info!("EOSE received for subscription {sub_id} via {relay_url}");
        }

        _ => {}
    }

    Ok(())
}

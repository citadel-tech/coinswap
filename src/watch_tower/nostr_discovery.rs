//! Discovers maker fidelity bond from Nostr relays
use std::{
    borrow::Cow,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
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
        rpc_backend::BitcoinRpc,
        utils::{parse_fidelity_event, process_fidelity},
        watcher_error::WatcherError,
    },
};

// ## TODO: Instead of looping over relay's have a connection Pool.
/// Discovers maker fidelity bonds by subscribing to Nostr events (kind 37777).
pub fn run_discovery(
    bitcoin_rpc: BitcoinRpc,
    registry: FileRegistry,
    shutdown: Arc<AtomicBool>,
) -> Result<(), WatcherError> {
    log::info!("Starting market discovery via Nostr");

    let registry = Arc::new(registry);
    let bitcoin_rpc = Arc::new(bitcoin_rpc);

    for relay in NOSTR_RELAYS {
        let relay = relay.to_string();
        let shutdown = shutdown.clone();
        let registry = Arc::clone(&registry);
        let bitcoin_rpc = Arc::clone(&bitcoin_rpc);

        std::thread::Builder::new()
            .name(format!("nostr-session-{}", relay))
            .spawn(move || {
                run_nostr_session_for_relay(&relay.clone(), registry, shutdown, bitcoin_rpc);
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
    bitcoin_rpc: Arc<BitcoinRpc>,
) {
    log::info!("Starting Nostr session for relay {}", relay_url);

    while !shutdown.load(Ordering::SeqCst) {
        match connect_and_run_once(
            relay_url,
            registry.clone(),
            shutdown.clone(),
            bitcoin_rpc.clone(),
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

/// Establishes a single Nostr connection and processes events until error or shutdown.
fn connect_and_run_once(
    relay_url: &str,
    registry: Arc<FileRegistry>,
    shutdown: Arc<AtomicBool>,
    bitcoin_rpc: Arc<BitcoinRpc>,
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

    read_event_loop(registry, socket, shutdown, bitcoin_rpc, relay_url)
}


fn read_event_loop(
    registry: Arc<FileRegistry>,
    mut socket: tungstenite::WebSocket<MaybeTlsStream<std::net::TcpStream>>,
    shutdown: Arc<AtomicBool>,
    bitcoin_rpc: Arc<BitcoinRpc>,
    relay_url: &str,
) -> Result<(), WatcherError> {
    while !shutdown.load(Ordering::SeqCst) {
        let msg = socket.read()?;

        let text = match msg {
            Message::Text(t) => t,
            Message::Binary(b) => String::from_utf8(b.to_vec())?.into(),
            _ => continue,
        };

        let relay_msg = RelayMessage::from_json(&text)?;

        handle_relay_message(registry.clone(), relay_msg, bitcoin_rpc.clone(), relay_url)?;
    }

    Ok(())
}

fn handle_relay_message(
    registry: Arc<FileRegistry>,
    msg: RelayMessage,
    bitcoin_rpc: Arc<BitcoinRpc>,
    relay_url: &str,
) -> Result<(), WatcherError> {
    match msg {
        RelayMessage::Event { event, .. } => {
            if event.kind != Kind::Custom(COINSWAP_KIND) {
                return Ok(());
            }

            let Some((txid, vout)) = parse_fidelity_event(&event) else {
                return Ok(());
            };

            // ## TODO: Optimize for this, we are currently doing a lot of RPC calls which
            //    are redundant as nostr relay's share same event multiple time. Come up
            //    with a clever way to reduce these RPC trips.
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
        }

        RelayMessage::EndOfStoredEvents(sub_id) => {
            log::info!("EOSE received for subscription {sub_id} via {relay_url}");
        }

        _ => {}
    }

    Ok(())
}

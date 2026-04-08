//! Nostr integration for Maker announcements and coordination.
//!
//! This module provides a minimal interface for publishing Maker-related
//! events over the Nostr protocol. It is primarily used to broadcast
//! fidelity bond information and other coordination signals required
//! by the Coinswap protocol.

use std::{
    net::TcpStream,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use nostr::{
    event::{EventBuilder, Kind, Tag, TagStandard},
    key::{Keys, SecretKey},
    message::{ClientMessage, RelayMessage},
    types::Timestamp,
    util::JsonUtil,
};
use socks::Socks5Stream;
use tungstenite::{stream::MaybeTlsStream, Message, WebSocket};

use crate::{maker::MakerError, protocol::common_messages::FidelityProof, utill::relay_host_port};

/// nostr url for coinswap
#[cfg(not(feature = "integration-test"))]
pub const NOSTR_RELAYS: &[&str] = &["wss://nos.lol", "wss://relay.damus.io"];
/// nostr url for coinswap
#[cfg(feature = "integration-test")]
pub const NOSTR_RELAYS: &[&str] = &["ws://127.0.0.1:8000"];

/// coinswap nostr event kind
pub const COINSWAP_KIND: u16 = 37778;
/// Expiration time for noster event (24 hours)
const EXPIRATION_SECS: u64 = 86400;

/// Opens a WebSocket connection to a Nostr relay.
///
/// In production, routes through the local Tor SOCKS5 proxy at `127.0.0.1:<socks_port>`.
/// Under `integration-test`, connects directly to allow local test relays.
fn connect_relay_ws(
    relay: &str,
    socks_port: u16,
) -> Result<WebSocket<MaybeTlsStream<TcpStream>>, MakerError> {
    if cfg!(feature = "integration-test") {
        return tungstenite::connect(relay).map(|(s, _)| s).map_err(|e| {
            log::warn!("failed to connect to nostr relay {}: {}", relay, e);
            MakerError::General("failed to connect to nostr relay")
        });
    }

    let host_port = relay_host_port(relay)
        .map_err(|_| MakerError::General("invalid relay URL: missing ws:// or wss:// scheme"))?;
    let socks_addr = format!("127.0.0.1:{}", socks_port);
    let tcp = Socks5Stream::connect(socks_addr.as_str(), host_port.as_str())
        .map_err(|e| {
            log::warn!(
                "failed to reach nostr relay {} via Tor SOCKS5 ({}): {}",
                relay,
                socks_addr,
                e
            );
            MakerError::General("failed to connect to nostr relay via Tor")
        })?
        .into_inner();

    tungstenite::client_tls_with_config(relay, tcp, None, None)
        .map(|(s, _)| s)
        .map_err(|e| {
            log::warn!("WebSocket handshake failed for {}: {}", relay, e);
            MakerError::General("WebSocket handshake failed for nostr relay")
        })
}

/// Broadcasts a fidelity bond announcement over Nostr.
pub fn broadcast_bond_on_nostr(
    fidelity: FidelityProof,
    relays: &[String],
    socks_port: u16,
) -> Result<(), MakerError> {
    let outpoint = fidelity.bond.outpoint;
    let content = format!("{}:{}", outpoint.txid, outpoint.vout);
    // Kind 37778 is in the NIP-33 parameterized-replaceable range (30000..39999),
    // so included a stable `d` tag to keep relay handling spec-compliant.
    let d_tag = format!("fidelity:{}", content);

    let secret_key = SecretKey::generate();
    let keys = Keys::new(secret_key);

    let expiration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| {
            log::warn!("failed to create expiration time : {}", e);
            MakerError::General("failed to create expiration time")
        })?
        .as_secs()
        + EXPIRATION_SECS;

    log::debug!(
        "Publishing fidelity bond to Nostr | outpoint={} amount_sats={} lock_time={} conf_height={:?} cert_expiry={:?} is_spent={} pubkey={} cert_hash={} cert_sig={:?} content={} d_tag={} expiration_unix={}",
        fidelity.bond.outpoint,
        fidelity.bond.amount.to_sat(),
        fidelity.bond.lock_time.to_consensus_u32(),
        fidelity.bond.conf_height,
        fidelity.bond.cert_expiry,
        fidelity.bond.is_spent,
        fidelity.bond.pubkey,
        fidelity.cert_hash,
        fidelity.cert_sig,
        content,
        d_tag,
        expiration
    );

    let event = EventBuilder::new(Kind::Custom(COINSWAP_KIND), content)
        .tag(Tag::identifier(d_tag))
        .tag(Tag::from_standardized(TagStandard::Expiration(
            Timestamp::from_secs(expiration),
        )))
        .build(keys.public_key)
        .sign_with_keys(&keys)
        .expect("Event should be signed");

    log::debug!(
        "Nostr event built | event_id={} pubkey={} kind={} created_at={} tags={:?}",
        event.id,
        event.pubkey,
        COINSWAP_KIND,
        event.created_at,
        event.tags
    );

    let msg = ClientMessage::Event(std::borrow::Cow::Owned(event));

    log::debug!("nostr wire msg: {}", msg.as_json());

    const RELAY_DELAY: Duration = Duration::from_secs(2);
    const MAX_RETRIES: usize = 3;

    let mut success = false;

    for relay in relays {
        for attempt in 1..=MAX_RETRIES {
            match broadcast_to_relay(relay, &msg, socks_port) {
                Ok(()) => {
                    success = true;
                    break;
                }
                Err(e) => {
                    log::warn!(
                        "failed to broadcast to {} (attempt {}/{}): {:?}",
                        relay,
                        attempt,
                        MAX_RETRIES,
                        e
                    );
                    if attempt < MAX_RETRIES {
                        std::thread::sleep(RELAY_DELAY);
                    }
                }
            }
        }
    }

    if !success {
        log::warn!("nostr event was not accepted by any relay");
    }

    Ok(())
}

/// Sends a Nostr event to a single relay and waits for confirmation.
fn broadcast_to_relay(relay: &str, msg: &ClientMessage, socks_port: u16) -> Result<(), MakerError> {
    let mut socket = connect_relay_ws(relay, socks_port)?;

    socket
        .write(Message::Text(msg.as_json().into()))
        .map_err(|e| {
            log::warn!("nostr relay write failed: {}", e);
            MakerError::General("failed to write to nostr relay")
        })?;
    socket.flush().ok();

    match socket.read() {
        Ok(Message::Text(text)) => {
            if let Ok(relay_msg) = RelayMessage::from_json(&text) {
                match relay_msg {
                    RelayMessage::Ok {
                        event_id,
                        status: true,
                        ..
                    } => {
                        log::info!("nostr relay {} accepted event {}", relay, event_id);
                        return Ok(());
                    }
                    RelayMessage::Ok {
                        event_id,
                        status: false,
                        message,
                    } => {
                        log::warn!(
                            "nostr relay {} rejected event {}: {}",
                            relay,
                            event_id,
                            message
                        );
                    }
                    _ => {}
                }
            }
        }
        Ok(_) => {}
        Err(e) => {
            log::warn!("nostr relay {} read error: {}", relay, e);
        }
    }
    log::warn!("nostr relay {} did not confirm event", relay);
    Err(MakerError::General("nostr relay did not confirm event"))
}

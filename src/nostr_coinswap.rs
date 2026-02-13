//! Nostr integration for Maker announcements and coordination.
//!
//! This module provides a minimal interface for publishing Maker-related
//! events over the Nostr protocol. It is primarily used to broadcast
//! fidelity bond information and other coordination signals required
//! by the Coinswap protocol.

use std::time::{SystemTime, UNIX_EPOCH};

use nostr::{
    event::{EventBuilder, Kind, Tag, TagStandard},
    key::{Keys, SecretKey},
    message::{ClientMessage, RelayMessage},
    types::Timestamp,
    util::JsonUtil,
};
#[cfg(not(feature = "integration-test"))]
use socks::Socks5Stream;
use tungstenite::Message;
#[cfg(not(feature = "integration-test"))]
use tungstenite::client_tls;
#[cfg(not(feature = "integration-test"))]
use tungstenite::http::Uri;

use crate::{maker::MakerError, protocol::messages::FidelityProof};

/// nostr url for coinswap
#[cfg(not(feature = "integration-test"))]
pub const NOSTR_RELAYS: &[&str] = &["wss://nos.lol", "wss://relay.damus.io"];
/// nostr url for coinswap
#[cfg(feature = "integration-test")]
pub const NOSTR_RELAYS: &[&str] = &["ws://127.0.0.1:8000"];

/// coinswap nostr event kind
pub const COINSWAP_KIND: u16 = 37777;
/// Expiration time for noster event (24 hours)
const EXPIRATION_SECS: u64 = 86400;

/// Broadcasts a fidelity bond announcement over Nostr.
pub fn broadcast_bond_on_nostr(fidelity: FidelityProof, socks_port: u16) -> Result<(), MakerError> {
    let outpoint = fidelity.bond.outpoint;
    let content = format!("{}:{}", outpoint.txid, outpoint.vout);

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

    let event = EventBuilder::new(Kind::Custom(COINSWAP_KIND), content)
        .tag(Tag::from_standardized(TagStandard::Expiration(
            Timestamp::from_secs(expiration),
        )))
        .build(keys.public_key)
        .sign_with_keys(&keys)
        .expect("Event should be signed");

    let msg = ClientMessage::Event(std::borrow::Cow::Owned(event));

    log::debug!("nostr wire msg: {}", msg.as_json());

    const MAX_RETRIES: usize = 3;
    const RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(2);

    let mut success = false;

    for relay in NOSTR_RELAYS {
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
                        std::thread::sleep(RETRY_DELAY);
                    }
                }
            }
        }
    }

    if !success {
        log::warn!("nostr event was not accepted by any relay");
        return Err(MakerError::General(
            "nostr event was not accepted by any relay",
        ));
    }

    Ok(())
}

#[cfg(not(feature = "integration-test"))]
fn parse_relay_endpoint(relay: &str) -> Result<(String, u16), MakerError> {
    let uri: Uri = relay.parse().map_err(|e| {
        log::warn!("invalid nostr relay url {}: {}", relay, e);
        MakerError::General("invalid nostr relay url")
    })?;

    let scheme = uri.scheme_str().ok_or_else(|| {
        log::warn!("missing scheme in nostr relay url {}", relay);
        MakerError::General("invalid nostr relay url")
    })?;

    if scheme != "ws" && scheme != "wss" {
        log::warn!("unsupported nostr relay scheme {} in {}", scheme, relay);
        return Err(MakerError::General("invalid nostr relay url"));
    }

    let host = uri.host().ok_or_else(|| {
        log::warn!("missing host in nostr relay url {}", relay);
        MakerError::General("invalid nostr relay url")
    })?;

    let port = uri
        .port_u16()
        .unwrap_or_else(|| if scheme == "wss" { 443 } else { 80 });

    Ok((host.to_string(), port))
}

/// Sends a Nostr event to a single relay and waits for confirmation.
#[cfg(feature = "integration-test")]
fn broadcast_to_relay(
    relay: &str,
    msg: &ClientMessage,
    _socks_port: u16,
) -> Result<(), MakerError> {
    let (mut socket, _) = tungstenite::connect(relay).map_err(|e| {
        log::warn!("failed to connect to nostr relay {}: {}", relay, e);
        MakerError::General("failed to connect to nostr relay")
    })?;

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

/// Sends a Nostr event to a single relay and waits for confirmation.
#[cfg(not(feature = "integration-test"))]
fn broadcast_to_relay(relay: &str, msg: &ClientMessage, socks_port: u16) -> Result<(), MakerError> {
    let (mut socket, _) = {
        let (host, port) = parse_relay_endpoint(relay)?;
        let proxy_addr = format!("127.0.0.1:{socks_port}");

        let stream = Socks5Stream::connect(proxy_addr.as_str(), (host.as_str(), port)).map_err(
            |e| {
                log::warn!(
                    "failed to connect to nostr relay {} through tor socks {}: {}",
                    relay,
                    proxy_addr,
                    e
                );
                MakerError::General("failed to connect to nostr relay via tor proxy")
            },
        )?;

        // Use TLS for wss:// and plain websocket for ws:// on top of the already-proxied stream.
        client_tls(relay, stream).map_err(|e| {
            log::warn!(
                "failed websocket handshake with nostr relay {} over tor proxy: {}",
                relay,
                e
            );
            MakerError::General("failed to complete nostr websocket handshake")
        })?
    };

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

#[cfg(all(test, not(feature = "integration-test")))]
mod tests {
    use super::parse_relay_endpoint;

    #[test]
    fn test_parse_relay_endpoint_defaults_ports() {
        let (host_ws, port_ws) = parse_relay_endpoint("ws://relay.example").unwrap();
        assert_eq!(host_ws, "relay.example");
        assert_eq!(port_ws, 80);

        let (host_wss, port_wss) = parse_relay_endpoint("wss://relay.example").unwrap();
        assert_eq!(host_wss, "relay.example");
        assert_eq!(port_wss, 443);
    }

    #[test]
    fn test_parse_relay_endpoint_explicit_port() {
        let (host, port) = parse_relay_endpoint("wss://relay.example:8443").unwrap();
        assert_eq!(host, "relay.example");
        assert_eq!(port, 8443);
    }

    #[test]
    fn test_parse_relay_endpoint_rejects_invalid_input() {
        assert!(parse_relay_endpoint("relay.example").is_err());
        assert!(parse_relay_endpoint("http://relay.example").is_err());
        assert!(parse_relay_endpoint("wss:///missing-host").is_err());
    }
}

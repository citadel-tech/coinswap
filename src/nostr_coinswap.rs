//! Nostr integration for Maker announcements and coordination.
//!
//! This module provides a minimal interface for publishing Maker-related
//! events over the Nostr protocol. It is primarily used to broadcast
//! fidelity bond information and other coordination signals required
//! by the Coinswap protocol.

use nostr::{
    event::{EventBuilder, Kind},
    key::{Keys, SecretKey},
    message::{ClientMessage, RelayMessage},
    util::JsonUtil,
};
use tungstenite::Message;

use crate::{maker::MakerError, protocol::messages::FidelityProof};

/// nost url for coinswap
#[cfg(not(feature = "integration-test"))]
pub const NOSTR_RELAYS: &[&str] = &["wss://nos.lol"];
/// nostr url for coinswap
#[cfg(feature = "integration-test")]
pub const NOSTR_RELAYS: &[&str] = &["ws://127.0.0.1:8000"];

/// coinswap nostr event kind
pub const COINSWAP_KIND: u16 = 37777;

/// Broadcasts a fidelity bond announcement over Nostr.
pub fn broadcast_bond_on_nostr(fidelity: FidelityProof) -> Result<(), MakerError> {
    let outpoint = fidelity.bond.outpoint;
    let content = format!("{}:{}", outpoint.txid, outpoint.vout);

    let secret_key = SecretKey::generate();
    let keys = Keys::new(secret_key);

    let event = EventBuilder::new(Kind::Custom(COINSWAP_KIND), content)
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
            match broadcast_to_relay(relay, &msg) {
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
    }

    Ok(())
}

/// Sends a Nostr event to a single relay and waits for confirmation.
fn broadcast_to_relay(relay: &str, msg: &ClientMessage) -> Result<(), MakerError> {
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

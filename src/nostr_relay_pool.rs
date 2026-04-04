//! Nostr relay connection pool.
//!
//! Provides a shared, reusable pool of WebSocket connections to Nostr relays.
//! Connections are created lazily on first use and transparently reconnected
//! when they fail. Each relay has its own lock so operations on different
//! relays never block each other.

use std::{
    collections::HashMap,
    io::ErrorKind,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, MutexGuard,
    },
    time::Duration,
};

use nostr::{
    message::{ClientMessage, RelayMessage},
    util::JsonUtil,
};
use tungstenite::{stream::MaybeTlsStream, Message, WebSocket};

/// Read timeout applied to every relay socket so blocking reads
/// can be interrupted periodically to check for shutdown.
const SOCKET_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Errors originating from the relay connection pool.
#[derive(Debug)]
pub enum RelayPoolError {
    /// WebSocket-level error (connect, read, write).
    WebSocket(tungstenite::Error),
    /// Nostr message could not be parsed.
    NostrParsing(nostr::message::MessageHandleError),
    /// UTF-8 decoding failed on a binary frame.
    Utf8(std::string::FromUtf8Error),
    /// The relay URL is not registered in this pool.
    UnknownRelay(String),
    /// The shutdown flag was set.
    Shutdown,
}

impl std::fmt::Display for RelayPoolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WebSocket(e) => write!(f, "websocket: {e}"),
            Self::NostrParsing(e) => write!(f, "nostr parse: {e}"),
            Self::Utf8(e) => write!(f, "utf8: {e}"),
            Self::UnknownRelay(url) => write!(f, "unknown relay: {url}"),
            Self::Shutdown => write!(f, "shutdown"),
        }
    }
}

impl From<tungstenite::Error> for RelayPoolError {
    fn from(e: tungstenite::Error) -> Self {
        Self::WebSocket(e)
    }
}

impl From<nostr::message::MessageHandleError> for RelayPoolError {
    fn from(e: nostr::message::MessageHandleError) -> Self {
        Self::NostrParsing(e)
    }
}

impl From<std::string::FromUtf8Error> for RelayPoolError {
    fn from(e: std::string::FromUtf8Error) -> Self {
        Self::Utf8(e)
    }
}

/// Returns `true` when the I/O error is a harmless read timeout
/// (the socket is still healthy).
fn is_timeout(e: &tungstenite::Error) -> bool {
    if let tungstenite::Error::Io(io) = e {
        matches!(io.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
    } else {
        false
    }
}

/// Per-relay connection state, protected by its own mutex.
struct RelayEntry {
    url: String,
    socket: Mutex<Option<WebSocket<MaybeTlsStream<std::net::TcpStream>>>>,
}

/// A pool of persistent WebSocket connections to Nostr relays.
///
/// The relay map is built at construction time and never modified,
/// so lookups are lock-free. Only individual relay sockets are behind
/// a `Mutex`, ensuring that operations on different relays are fully
/// independent.
pub struct RelayPool {
    relays: HashMap<String, Arc<RelayEntry>>,
    shutdown: Arc<AtomicBool>,
}

impl RelayPool {
    /// Creates a new pool for the given relay URLs.
    /// Connections are established lazily on first use.
    pub fn new(relay_urls: &[String], shutdown: Arc<AtomicBool>) -> Self {
        let relays = relay_urls
            .iter()
            .map(|url| {
                let entry = Arc::new(RelayEntry {
                    url: url.clone(),
                    socket: Mutex::new(None),
                });
                (url.clone(), entry)
            })
            .collect();
        Self { relays, shutdown }
    }

    /// Returns the list of relay URLs registered in this pool.
    pub fn relay_urls(&self) -> Vec<&str> {
        self.relays.keys().map(|s| s.as_str()).collect()
    }

    /// Sends a Nostr client message to a single relay.
    ///
    /// Reconnects transparently if the socket is absent or broken.
    /// On a real write error the socket is marked dead for the next call.
    pub fn send(&self, relay_url: &str, msg: &ClientMessage) -> Result<(), RelayPoolError> {
        if self.shutdown.load(Ordering::Acquire) {
            return Err(RelayPoolError::Shutdown);
        }

        let entry = self
            .relays
            .get(relay_url)
            .ok_or_else(|| RelayPoolError::UnknownRelay(relay_url.to_string()))?;

        let mut guard = Self::lock_entry(entry)?;
        let socket = Self::ensure_connected(&mut guard, &entry.url)?;

        match socket.write(Message::Text(msg.as_json().into())) {
            Ok(()) => {
                socket.flush().ok();
                Ok(())
            }
            Err(e) => {
                *guard = None; // mark dead
                Err(RelayPoolError::WebSocket(e))
            }
        }
    }

    /// Reads the next Nostr relay message from a single relay.
    ///
    /// Blocks until a message arrives or the read timeout fires.
    /// Returns `Err(RelayPoolError::Shutdown)` when shutdown is requested
    /// and the read timed out. On a real read error the socket is marked dead.
    pub fn read(&self, relay_url: &str) -> Result<RelayMessage<'_>, RelayPoolError> {
        if self.shutdown.load(Ordering::Acquire) {
            return Err(RelayPoolError::Shutdown);
        }

        let entry = self
            .relays
            .get(relay_url)
            .ok_or_else(|| RelayPoolError::UnknownRelay(relay_url.to_string()))?;

        let mut guard = Self::lock_entry(entry)?;
        let socket = Self::ensure_connected(&mut guard, &entry.url)?;

        match socket.read() {
            Ok(msg) => {
                // Release the lock before parsing.
                let text = match msg {
                    Message::Text(t) => t,
                    Message::Binary(b) => String::from_utf8(b.to_vec())?.into(),
                    _ => {
                        // ping/pong/close frames — re-check shutdown
                        drop(guard);
                        if self.shutdown.load(Ordering::Acquire) {
                            return Err(RelayPoolError::Shutdown);
                        }
                        return Err(RelayPoolError::WebSocket(tungstenite::Error::Protocol(
                            tungstenite::error::ProtocolError::ResetWithoutClosingHandshake,
                        )));
                    }
                };
                drop(guard);
                Ok(RelayMessage::from_json(&text)?)
            }
            Err(e) if is_timeout(&e) => {
                // Socket is healthy, just no data yet.
                drop(guard);
                if self.shutdown.load(Ordering::Acquire) {
                    return Err(RelayPoolError::Shutdown);
                }
                Err(RelayPoolError::WebSocket(e))
            }
            Err(e) => {
                *guard = None; // mark dead
                Err(RelayPoolError::WebSocket(e))
            }
        }
    }

    /// Sends a message and reads one response. Convenience for the broadcast path.
    pub fn send_and_read_response(
        &self,
        relay_url: &str,
        msg: &ClientMessage,
    ) -> Result<RelayMessage<'_>, RelayPoolError> {
        if self.shutdown.load(Ordering::Acquire) {
            return Err(RelayPoolError::Shutdown);
        }

        let entry = self
            .relays
            .get(relay_url)
            .ok_or_else(|| RelayPoolError::UnknownRelay(relay_url.to_string()))?;

        let mut guard = Self::lock_entry(entry)?;
        let socket = Self::ensure_connected(&mut guard, &entry.url)?;

        // Write
        if let Err(e) = socket.write(Message::Text(msg.as_json().into())) {
            *guard = None;
            return Err(RelayPoolError::WebSocket(e));
        }
        socket.flush().ok();

        // Read one response
        match socket.read() {
            Ok(Message::Text(text)) => {
                drop(guard);
                Ok(RelayMessage::from_json(&text)?)
            }
            Ok(_) => {
                drop(guard);
                Err(RelayPoolError::WebSocket(tungstenite::Error::Protocol(
                    tungstenite::error::ProtocolError::ResetWithoutClosingHandshake,
                )))
            }
            Err(e) if is_timeout(&e) => {
                // Timeout waiting for relay confirmation — not fatal to socket but
                // caller should treat as "relay did not confirm".
                drop(guard);
                Err(RelayPoolError::WebSocket(e))
            }
            Err(e) => {
                *guard = None;
                Err(RelayPoolError::WebSocket(e))
            }
        }
    }

    /// Drops the connection for a single relay (it will reconnect lazily).
    pub fn disconnect(&self, relay_url: &str) {
        if let Some(entry) = self.relays.get(relay_url) {
            if let Ok(mut guard) = entry.socket.lock() {
                *guard = None;
            }
        }
    }

    /// Drops all connections.
    pub fn disconnect_all(&self) {
        for entry in self.relays.values() {
            if let Ok(mut guard) = entry.socket.lock() {
                *guard = None;
            }
        }
    }

    // -- private helpers --

    /// Locks the relay entry, recovering from mutex poisoning by
    /// resetting the socket to `None`.
    fn lock_entry(
        entry: &RelayEntry,
    ) -> Result<
        MutexGuard<'_, Option<WebSocket<MaybeTlsStream<std::net::TcpStream>>>>,
        RelayPoolError,
    > {
        match entry.socket.lock() {
            Ok(guard) => Ok(guard),
            Err(poisoned) => {
                // Recover: take the guard and reset to disconnected.
                let mut guard = poisoned.into_inner();
                *guard = None;
                Ok(guard)
            }
        }
    }

    /// Ensures the guarded socket is connected. If it is `None`, opens
    /// a new WebSocket and sets a read timeout on the underlying TCP stream.
    fn ensure_connected<'a>(
        guard: &'a mut MutexGuard<'_, Option<WebSocket<MaybeTlsStream<std::net::TcpStream>>>>,
        url: &str,
    ) -> Result<&'a mut WebSocket<MaybeTlsStream<std::net::TcpStream>>, RelayPoolError> {
        if guard.is_none() {
            log::debug!("Connecting to relay {}", url);
            let (socket, _) = tungstenite::connect(url)?;

            // Set read timeout so blocking reads can be interrupted for shutdown checks.
            Self::set_read_timeout(&socket);

            **guard = Some(socket);
        }
        Ok(guard.as_mut().expect("just ensured Some"))
    }

    /// Applies a read timeout on the underlying TCP stream, regardless of
    /// whether the connection is plain or TLS-wrapped.
    fn set_read_timeout(socket: &WebSocket<MaybeTlsStream<std::net::TcpStream>>) {
        let timeout = Some(SOCKET_READ_TIMEOUT);
        match socket.get_ref() {
            MaybeTlsStream::Plain(tcp) => {
                tcp.set_read_timeout(timeout).ok();
            }
            MaybeTlsStream::NativeTls(tls) => {
                tls.get_ref().set_read_timeout(timeout).ok();
            }
            _ => {}
        }
    }
}

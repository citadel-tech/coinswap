//! Watchtower-related error types.

use std::sync::{MutexGuard, PoisonError};

/// Errors that can occur within the watchtower components.
#[derive(Debug)]
pub enum WatcherError {
    /// Internal server failure.
    ServerError,
    /// Failure in the mempool indexer.
    MempoolIndexerError,
    /// Graceful shutdown requested.
    Shutdown,
    /// Transaction or block parsing failed.
    ParsingError,
    /// Channel send failed.
    SendError,
    /// I/O error surfaced from filesystem operations.
    IOError(std::io::Error),
    /// RPC error from bitcoind.
    RPCError(bitcoind::bitcoincore_rpc::Error),
    /// Serialization/deserialization error for CBOR.
    SerdeCbor(serde_cbor::Error),
    /// WebSocket error from tungstenite
    WebSocket(tungstenite::Error),
    /// Nostr message parsing error
    NostrParsingError(nostr::message::MessageHandleError),
    /// Represents a mutex poisoning error.
    MutexPoison,
    /// Represents a general error with a descriptive message.
    General(String),
}

impl From<std::io::Error> for WatcherError {
    fn from(value: std::io::Error) -> Self {
        WatcherError::IOError(value)
    }
}

impl From<bitcoind::bitcoincore_rpc::Error> for WatcherError {
    fn from(value: bitcoind::bitcoincore_rpc::Error) -> Self {
        WatcherError::RPCError(value)
    }
}

impl std::fmt::Display for WatcherError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl From<serde_cbor::Error> for WatcherError {
    fn from(value: serde_cbor::Error) -> Self {
        Self::SerdeCbor(value)
    }
}

impl From<tungstenite::Error> for WatcherError {
    fn from(value: tungstenite::Error) -> Self {
        WatcherError::WebSocket(value)
    }
}

impl From<std::string::FromUtf8Error> for WatcherError {
    fn from(_value: std::string::FromUtf8Error) -> Self {
        WatcherError::ParsingError
    }
}

impl From<nostr::message::MessageHandleError> for WatcherError {
    fn from(value: nostr::message::MessageHandleError) -> Self {
        WatcherError::NostrParsingError(value)
    }
}

impl<'a, T> From<PoisonError<MutexGuard<'a, T>>> for WatcherError {
    fn from(_: PoisonError<MutexGuard<'a, T>>) -> Self {
        Self::MutexPoison
    }
}

impl WatcherError {
    /// Returns the underlying `ErrorKind` if the error wraps an I/O failure.
    pub fn io_error_kind(&self) -> Option<std::io::ErrorKind> {
        match self {
            WatcherError::IOError(e) => Some(e.kind()),
            _ => None,
        }
    }

    /// Returns a stable string identifier for the error variant.
    pub fn kind(&self) -> &'static str {
        match self {
            WatcherError::ServerError => "ServerError",
            WatcherError::MempoolIndexerError => "MempoolIndexerError",
            WatcherError::Shutdown => "Shutdown",
            WatcherError::ParsingError => "ParsingError",
            WatcherError::SendError => "SendError",
            WatcherError::IOError(_) => "IOError",
            WatcherError::RPCError(_) => "RPCError",
            WatcherError::SerdeCbor(_) => "SerdeCbor",
            WatcherError::WebSocket(_) => "WebSocket",
            WatcherError::NostrParsingError(_) => "NostrParsingError",
            WatcherError::MutexPoison => "MutexPoison",
            WatcherError::General(_) => "General",
        }
    }
}

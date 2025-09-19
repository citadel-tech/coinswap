//! High-level network and protocol errors.

use std::error::Error;

/// Represents all possible network-related errors.
#[derive(Debug)]
pub enum NetError {
    /// Error originating from standard I/O operations.
    ///
    /// This variant wraps a [`std::io::Error`] to provide details about I/O failures.
    IO(std::io::Error),

    /// Error indicating the end of a file was reached unexpectedly.
    ReachedEOF,

    /// Error indicating that a connection attempt timed out.
    ConnectionTimedOut,

    /// Error caused by an invalid network address.
    InvalidNetworkAddress,

    /// Error related to CBOR (Concise Binary Object Representation) serialization or deserialization.
    ///
    /// This variant wraps a [`serde_cbor::Error`] to provide details about the issue.
    Cbor(serde_cbor::Error),

    /// Error indicating an invalid CLI application network.
    InvalidAppNetwork,
}

impl std::fmt::Display for NetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl Error for NetError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        None
    }
}

impl From<std::io::Error> for NetError {
    fn from(value: std::io::Error) -> Self {
        Self::IO(value)
    }
}

impl From<serde_cbor::Error> for NetError {
    fn from(value: serde_cbor::Error) -> Self {
        Self::Cbor(value)
    }
}

/// Represents various errors that can occur while doing Fee Estimation
#[derive(Debug)]
pub enum FeeEstimatorError {
    /// Error from Bitcoin Core RPC
    BitcoinRpc(bitcoind::bitcoincore_rpc::Error),
    /// Error while receiving or parsing an HTTP Response
    HttpError(minreq::Error),
    /// Missing expected data in API response
    MissingData(String),
    /// No wallet configured for Bitcoin Core estimates
    NoWallet,
    /// No sources available or all sources failed
    NoFeeSources,
    /// A scoped thread panicked
    ThreadError,
}

impl From<bitcoind::bitcoincore_rpc::Error> for FeeEstimatorError {
    fn from(err: bitcoind::bitcoincore_rpc::Error) -> Self {
        FeeEstimatorError::BitcoinRpc(err)
    }
}

impl From<minreq::Error> for FeeEstimatorError {
    fn from(err: minreq::Error) -> Self {
        FeeEstimatorError::HttpError(err)
    }
}

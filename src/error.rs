//! High-level network and protocol errors.
//!
//! Provides error types for:
//! - Network operations (NetError)
//! - Protocol violations (ProtocolError)
//!
use std::error::Error;

use bitcoin::Amount;

use crate::protocol::error::ContractError;

/// Network-related errors that can occur during connections and data transfer.
///
/// Encapsulates errors from:
/// - IO operations
/// - Connection handling
/// - Data serialization
/// - Network configuration
#[derive(Debug)]
pub enum NetError {
    /// Standard IO errors during network operations.
    IO(std::io::Error),
    /// Connection closed unexpectedly, reached end of stream.
    ReachedEOF,
    /// Connection timed out waiting for response.
    ConnectionTimedOut,
    /// Failed to parse or validate network address.
    InvalidNetworkAddress,
    /// CBOR serialization or deserialization error.
    Cbor(serde_cbor::Error),
    /// Mismatched or invalid application network type.
    InvalidAppNetwork,
}
impl std::fmt::Display for NetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
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

/// Protocol-level errors that can occur during swap execution.
///
/// Encapsulates errors from:
/// - Message sequencing
/// - Signature validation
/// - Transaction verification
/// - Contract operations
#[derive(Debug)]
pub enum ProtocolError {
    /// Received unexpected message in protocol sequence.
    WrongMessage { expected: String, received: String },
    /// Incorrect number of signatures provided.
    WrongNumOfSigs { expected: usize, received: usize },
    /// Mismatch in number of contract transactions.
    WrongNumOfContractTxs { expected: usize, received: usize },
    /// Mismatch in number of private keys.
    WrongNumOfPrivkeys { expected: usize, received: usize },
    /// Amount funded doesn't match expected value.
    IncorrectFundingAmount { expected: Amount, found: Amount },
    /// Error in contract creation or validation.
    Contract(ContractError),
}

impl From<ContractError> for ProtocolError {
    fn from(value: ContractError) -> Self {
        Self::Contract(value)
    }
}

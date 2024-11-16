//! All Contract related errors.

use bitcoin::secp256k1;

/// Errors that can occur during contract operations.
///
/// Encapsulates errors from:
/// - Cryptographic operations
/// - Protocol violations
/// - Script handling
/// - Hash computations
#[derive(Debug)]
pub enum ContractError {
    /// Secp256k1 cryptographic errors.
    Secp(secp256k1::Error),
    /// Protocol violation with static message.
    Protocol(&'static str),
    /// Bitcoin script parsing or execution errors.
    Script(bitcoin::blockdata::script::Error),
    /// Hash slice conversion errors.
    Hash(bitcoin::hashes::FromSliceError),
    /// Key slice conversion errors.
    Key(bitcoin::key::FromSliceError),
    /// Signature hash computation errors.
    Sighash(bitcoin::transaction::InputsIndexError),
}

impl From<secp256k1::Error> for ContractError {
    fn from(value: secp256k1::Error) -> Self {
        Self::Secp(value)
    }
}

impl From<bitcoin::blockdata::script::Error> for ContractError {
    fn from(value: bitcoin::blockdata::script::Error) -> Self {
        Self::Script(value)
    }
}

impl From<bitcoin::hashes::FromSliceError> for ContractError {
    fn from(value: bitcoin::hashes::FromSliceError) -> Self {
        Self::Hash(value)
    }
}

impl From<bitcoin::key::FromSliceError> for ContractError {
    fn from(value: bitcoin::key::FromSliceError) -> Self {
        Self::Key(value)
    }
}

impl From<bitcoin::transaction::InputsIndexError> for ContractError {
    fn from(value: bitcoin::transaction::InputsIndexError) -> Self {
        Self::Sighash(value)
    }
}

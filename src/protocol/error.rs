//! All Contract related errors.

use bitcoin::{secp256k1, Amount};

/// Represents errors encountered during protocol operations.
///
/// This enum encapsulates various errors that can occur during protocol execution,
/// including cryptographic errors, mismatches in expected values, and general protocol violations.
#[derive(Debug)]
pub enum ProtocolError {
    /// Error related to Secp256k1 cryptographic operations.
    Secp(secp256k1::Error),
    /// Error in Bitcoin script handling.
    Script(bitcoin::blockdata::script::Error),
    /// Error converting from a byte slice to a hash type.
    Hash(bitcoin::hashes::FromSliceError),
    /// Error converting from a byte slice to a key type.
    Key(bitcoin::key::FromSliceError),
    /// Error related to calculating or validating Sighashes.
    Sighash(bitcoin::transaction::InputsIndexError),
    /// Error when an unexpected message is received.
    WrongMessage {
        /// The expected message type.
        expected: String,
        /// The received message type.
        received: String,
    },
    /// Error when the number of signatures does not match the expected count.
    WrongNumOfSigs {
        /// The expected number of signatures.
        expected: usize,
        /// The received number of signatures.
        received: usize,
    },
    /// Error when the number of contract transactions is incorrect.
    WrongNumOfContractTxs {
        /// The expected number of contract transactions.
        expected: usize,
        /// The received number of contract transactions.
        received: usize,
    },
    /// Error when the number of private keys is incorrect.
    WrongNumOfPrivkeys {
        /// The expected number of private keys.
        expected: usize,
        /// The received number of private keys.
        received: usize,
    },
    /// Error when the funding amount does not match the expected value.
    IncorrectFundingAmount {
        /// The expected funding amount.
        expected: Amount,
        /// The actual funding amount.
        found: Amount,
    },
    /// Error encountered when a non-segwit `script_pubkey` is used.
    ///
    /// The protocol only supports `V0_Segwit` transactions.
    ScriptPubkey(bitcoin::script::witness_program::Error),
    /// General error not covered by other variants.
    General(&'static str),
}

impl From<bitcoin::script::witness_program::Error> for ProtocolError {
    fn from(value: bitcoin::script::witness_program::Error) -> Self {
        Self::ScriptPubkey(value)
    }
}

impl From<secp256k1::Error> for ProtocolError {
    fn from(value: secp256k1::Error) -> Self {
        Self::Secp(value)
    }
}

impl From<bitcoin::blockdata::script::Error> for ProtocolError {
    fn from(value: bitcoin::blockdata::script::Error) -> Self {
        Self::Script(value)
    }
}

impl From<bitcoin::hashes::FromSliceError> for ProtocolError {
    fn from(value: bitcoin::hashes::FromSliceError) -> Self {
        Self::Hash(value)
    }
}

impl From<bitcoin::key::FromSliceError> for ProtocolError {
    fn from(value: bitcoin::key::FromSliceError) -> Self {
        Self::Key(value)
    }
}

impl From<bitcoin::transaction::InputsIndexError> for ProtocolError {
    fn from(value: bitcoin::transaction::InputsIndexError) -> Self {
        Self::Sighash(value)
    }
}

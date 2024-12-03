//! All Contract related errors.

use bitcoin::{secp256k1, Amount};

/// Includes all Protocol-level errors.
#[derive(Debug)]
pub enum ProtocolError {
    Secp(secp256k1::Error),
    Script(bitcoin::blockdata::script::Error),
    Hash(bitcoin::hashes::FromSliceError),
    Key(bitcoin::key::FromSliceError),
    Sighash(bitcoin::transaction::InputsIndexError),
    WrongMessage { expected: String, received: String },
    WrongNumOfSigs { expected: usize, received: usize },
    WrongNumOfContractTxs { expected: usize, received: usize },
    WrongNumOfPrivkeys { expected: usize, received: usize },
    IncorrectFundingAmount { expected: Amount, found: Amount },
    // This is returned if we ever encounter a non-segwit script_pubkey. The protocol only works with V0_Segwit transactions.
    ScriptPubkey(bitcoin::script::witness_program::Error),
    // Any other error not included in the above list
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

//! All Contract related errors.

use bitcoin::secp256k1;

/// Enum for handling contract-related errors.
#[derive(Debug)]
pub enum ContractError {
    Secp(secp256k1::Error),
    Protocol(&'static str),
    Script(bitcoin::blockdata::script::Error),
    Hash(bitcoin::hashes::FromSliceError),
    Key(bitcoin::key::FromSliceError),
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

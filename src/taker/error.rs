//! All Taker-related errors.
use crate::{
    error::NetError, market::directory::DirectoryServerError, protocol::error::ProtocolError,
    wallet::WalletError,
};

/// Enum for handling taker-related errors.
#[derive(Debug)]
pub enum TakerError {
    IO(std::io::Error),
    ContractsBroadcasted(Vec<bitcoin::Txid>),
    NotEnoughMakersInOfferBook,
    Wallet(WalletError),
    Directory(DirectoryServerError),
    Net(NetError),
    SendAmountNotSet,
    FundingTxWaitTimeOut,
    Deserialize(serde_cbor::Error),
    // MPSC channel failure error. Only occurs in internal thread communications.
    MPSC(String),
}

impl From<serde_cbor::Error> for TakerError {
    fn from(value: serde_cbor::Error) -> Self {
        Self::Deserialize(value)
    }
}

impl From<WalletError> for TakerError {
    fn from(value: WalletError) -> Self {
        Self::Wallet(value)
    }
}

impl From<DirectoryServerError> for TakerError {
    fn from(value: DirectoryServerError) -> Self {
        Self::Directory(value)
    }
}

impl From<std::io::Error> for TakerError {
    fn from(value: std::io::Error) -> Self {
        Self::IO(value)
    }
}

impl From<NetError> for TakerError {
    fn from(value: NetError) -> Self {
        Self::Net(value)
    }
}

impl From<ProtocolError> for TakerError {
    fn from(value: ProtocolError) -> Self {
        Self::Wallet(value.into())
    }
}

impl From<std::sync::mpsc::RecvError> for TakerError {
    fn from(value: std::sync::mpsc::RecvError) -> Self {
        Self::MPSC(value.to_string())
    }
}

impl<T> From<std::sync::mpsc::SendError<T>> for TakerError {
    fn from(value: std::sync::mpsc::SendError<T>) -> Self {
        Self::MPSC(value.to_string())
    }
}

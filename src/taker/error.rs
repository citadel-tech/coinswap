//! All Taker-related errors.
use crate::{
    error::NetError, market::directory::DirectoryServerError, protocol::error::ProtocolError,
    utill::TorError, wallet::WalletError,
};

/// Represents errors that can occur during Taker operations.
///
/// This enum covers a range of errors related to I/O, wallet operations, network communication,
/// and other Taker-specific scenarios.
#[derive(Debug)]
pub enum TakerError {
    /// Standard input/output error.
    IO(std::io::Error),
    /// Error indicating contracts were broadcasted prematurely.
    /// Contains a list of the transaction IDs of the broadcasted contracts.
    ContractsBroadcasted(Vec<bitcoin::Txid>),
    /// Error indicating there are not enough makers available in the offer book.
    NotEnoughMakersInOfferBook,
    /// Error related to wallet operations.
    Wallet(WalletError),
    /// Error encountered during interaction with the directory server.
    Directory(DirectoryServerError),
    /// Error related to network operations.
    Net(NetError),
    /// Error indicating the send amount was not set for a transaction.
    SendAmountNotSet,
    /// Error indicating a timeout while waiting for the funding transaction.
    FundingTxWaitTimeOut,
    /// Error deserializing data, typically related to CBOR-encoded data.
    Deserialize(String),
    /// Error indicating an MPSC channel failure.
    ///
    /// This error occurs during internal thread communication.
    MPSC(String),
    /// Tor error
    TorError(TorError),
}

impl From<TorError> for TakerError {
    fn from(value: TorError) -> Self {
        Self::TorError(value)
    }
}

impl From<serde_cbor::Error> for TakerError {
    fn from(value: serde_cbor::Error) -> Self {
        Self::Deserialize(value.to_string())
    }
}

impl From<serde_json::Error> for TakerError {
    fn from(value: serde_json::Error) -> Self {
        Self::Deserialize(value.to_string())
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

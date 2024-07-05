//! All Taker-related errors.

use bitcoin::Txid;

use bitcoind::bitcoincore_rpc::Error as RpcError;

use crate::{
    error::{NetError, ProtocolError},
    market::directory::DirectoryServerError,
    wallet::WalletError,
};

/// Enum for handling taker-related errors.
#[derive(Debug)]
pub enum TakerError {
    IO(std::io::Error),
    ContractsBroadcasted(Vec<Txid>),
    RPCError(RpcError),
    NotEnoughMakersInOfferBook,
    Wallet(WalletError),
    Directory(DirectoryServerError),
    Net(NetError),
    Socks(tokio_socks::Error),
    Protocol(ProtocolError),
    SendAmountNotSet,
    FundingTxWaitTimeOut,
    Deserialize(serde_cbor::Error),
}

impl From<serde_cbor::Error> for TakerError {
    fn from(value: serde_cbor::Error) -> Self {
        Self::Deserialize(value)
    }
}

impl From<RpcError> for TakerError {
    fn from(value: RpcError) -> Self {
        Self::RPCError(value)
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

impl From<tokio_socks::Error> for TakerError {
    fn from(value: tokio_socks::Error) -> Self {
        Self::Socks(value)
    }
}

impl From<ProtocolError> for TakerError {
    fn from(value: ProtocolError) -> Self {
        Self::Protocol(value)
    }
}

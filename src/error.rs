use std::{error, io};

use crate::{
    maker::error::MakerError, market::directory::DirectoryServerError,
    protocol::error::ContractError, wallet::WalletError,
};

// error enum for the whole project
// try to make functions return this
#[derive(Debug)]
pub enum TeleportError {
    Network(Box<dyn error::Error + Send>),
    Disk(io::Error),
    Protocol(&'static str),
    Rpc(bitcoind::bitcoincore_rpc::Error),
    Socks(tokio_socks::Error),
    Wallet(WalletError),
    Market(DirectoryServerError),
    Json(serde_json::Error),
    Maker(MakerError),
    Contract(ContractError),
}

impl From<Box<dyn error::Error + Send>> for TeleportError {
    fn from(e: Box<dyn error::Error + Send>) -> TeleportError {
        TeleportError::Network(e)
    }
}

impl From<io::Error> for TeleportError {
    fn from(e: io::Error) -> TeleportError {
        TeleportError::Disk(e)
    }
}

impl From<bitcoind::bitcoincore_rpc::Error> for TeleportError {
    fn from(e: bitcoind::bitcoincore_rpc::Error) -> TeleportError {
        TeleportError::Rpc(e)
    }
}

impl From<tokio_socks::Error> for TeleportError {
    fn from(e: tokio_socks::Error) -> TeleportError {
        TeleportError::Socks(e)
    }
}

impl From<WalletError> for TeleportError {
    fn from(value: WalletError) -> Self {
        Self::Wallet(value)
    }
}

impl From<DirectoryServerError> for TeleportError {
    fn from(value: DirectoryServerError) -> Self {
        Self::Market(value)
    }
}

impl From<serde_json::Error> for TeleportError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

impl From<MakerError> for TeleportError {
    fn from(value: MakerError) -> Self {
        Self::Maker(value)
    }
}

impl From<ContractError> for TeleportError {
    fn from(value: ContractError) -> Self {
        Self::Contract(value)
    }
}

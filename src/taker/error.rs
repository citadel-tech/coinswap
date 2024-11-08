//! All Taker-related errors.

use bitcoin::Txid;

use bitcoind::bitcoincore_rpc::Error as RpcError;

use crate::{
    error::{NetError, ProtocolError},
    market::directory::DirectoryServerError,
    wallet::WalletError,
};

/// Represents errors that can occur during taker operations.
///
/// Encapsulates errors from:
/// - Network and IO operations
/// - Contract handling
/// - Maker communications
/// - Wallet operations
/// - Protocol execution
#[derive(Debug)]
pub enum TakerError {
    /// Standard IO errors during file operations.
    IO(std::io::Error),
    /// Contract transactions were prematurely broadcast.
    ContractsBroadcasted(Vec<Txid>),
    /// RPC communication failures.
    RPCError(RpcError),
    /// Insufficient makers available for swap route.
    NotEnoughMakersInOfferBook,
    /// Errors from wallet operations like signing or broadcasting.
    Wallet(WalletError),
    /// Directory server communication failures.
    Directory(DirectoryServerError),
    /// Network-related errors during connections.
    Net(NetError),
    /// Protocol violation or swap execution errors.
    Protocol(ProtocolError),
    /// Swap amount not configured before execution.
    SendAmountNotSet,
    /// Timeout while waiting for funding transaction.
    FundingTxWaitTimeOut,
    /// CBOR serialization or deserialization errors.
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

impl From<ProtocolError> for TakerError {
    fn from(value: ProtocolError) -> Self {
        Self::Protocol(value)
    }
}

//! The Coinswap Wallet (unsecured). Used by both the Taker and Maker.

mod api;
mod direct_send;
mod error;
mod fidelity;
mod funding;
mod rpc;
mod storage;
mod swapcoin;
mod cbf;

pub use api::{DisplayAddressType, UTXOSpendInfo, Wallet};
pub use direct_send::{CoinToSpend, Destination, SendAmount};
pub use error::WalletError;
pub use fidelity::{FidelityBond, FidelityError};
pub use rpc::RPCConfig;
pub use storage::WalletStore;
pub use swapcoin::{
    IncomingSwapCoin, OutgoingSwapCoin, SwapCoin, WalletSwapCoin, WatchOnlySwapCoin,
};

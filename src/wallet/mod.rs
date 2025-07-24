//! The Coinswap Wallet (unsecured). Used by both the Taker and Maker.

mod api;
mod error;
mod fidelity;
mod funding;
mod rpc;
mod spend;
mod split_utxos;
mod storage;
mod swapcoin;

pub use api::Balances;
pub(crate) use api::{UTXOSpendInfo, Wallet};
pub use error::WalletError;
pub(crate) use fidelity::{fidelity_redeemscript, FidelityBond, FidelityError};
pub use rpc::RPCConfig;
pub use spend::Destination;
pub(crate) use swapcoin::{
    IncomingSwapCoin, OutgoingSwapCoin, SwapCoin, WalletSwapCoin, WatchOnlySwapCoin,
};

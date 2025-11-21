//! The Coinswap Wallet (unsecured). Used by both the Taker and Maker.

mod api;
mod backup;
mod error;
mod fidelity;
mod funding;
mod rpc;
mod spend;
mod split_utxos;
mod storage;
mod swapcoin;
mod swapcoin2;

pub use api::{Balances, UTXOSpendInfo, Wallet};
pub use backup::WalletBackup;
pub use error::WalletError;
pub use fidelity::FidelityBond;
pub(crate) use fidelity::{fidelity_redeemscript, FidelityError};
pub use rpc::RPCConfig;
pub use spend::Destination;
pub(crate) use swapcoin::{
    IncomingSwapCoin, OutgoingSwapCoin, SwapCoin, WalletSwapCoin, WatchOnlySwapCoin,
};
pub(crate) use swapcoin2::{IncomingSwapCoinV2, OutgoingSwapCoinV2};

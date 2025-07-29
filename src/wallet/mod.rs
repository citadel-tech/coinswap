//! The Coinswap Wallet (unsecured). Used by both the Taker and Maker.

// mod api;
mod api2;
mod error;
mod fidelity;
mod funding;
mod rpc;
mod spend;
mod storage;
mod swapcoin;
mod swapcoin2;

pub use api2::Wallet as TaprootWallet;
pub(crate) use api2::{Balances, KeychainKind, UTXOSpendInfo, Wallet};
pub use error::WalletError;
pub(crate) use fidelity::{fidelity_redeemscript, FidelityBond, FidelityError};
pub use rpc::RPCConfig;
pub use spend::Destination;
pub(crate) use swapcoin::{
    IncomingSwapCoin, OutgoingSwapCoin, SwapCoin, WalletSwapCoin, WatchOnlySwapCoin,
};
pub(crate) use swapcoin2::{
    IncomingSwapCoin as IncomingSwapCoin2, OutgoingSwapCoin as OutgoingSwapCoin2,
    SwapCoin as SwapCoin2, WalletSwapCoin as WalletSwapCoin2,
    WatchOnlySwapCoin as WatchOnlySwapCoin2,
};

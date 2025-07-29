//! The Coinswap Wallet (unsecured). Used by both the Taker and Maker.

mod api;
mod error;
mod fidelity;
mod funding;
mod rpc;
mod security;
mod spend;
mod split_utxos;
mod storage;
mod swapcoin;

pub(crate) use api::UTXOSpendInfo;
pub use api::{Balances, Wallet, WalletBackup};
pub use error::WalletError;
pub(crate) use fidelity::{fidelity_redeemscript, FidelityBond, FidelityError};
pub use rpc::RPCConfig;
pub use security::{load_sensitive_struct_interactive, KeyMaterial, SerdeCbor, SerdeJson};
pub use spend::Destination;
pub(crate) use swapcoin::{
    IncomingSwapCoin, OutgoingSwapCoin, SwapCoin, WalletSwapCoin, WatchOnlySwapCoin,
};

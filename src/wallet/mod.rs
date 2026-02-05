//! The Coinswap Wallet (unsecured). Used by both the Taker and Maker.

mod api;
mod backup;
mod error;
pub mod ffi;
mod fidelity;
mod funding;
pub mod reports;
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
pub use reports::{
    persist_maker_report, persist_taker_report, MakerFeeInfo, MakerSwapReport,
    MakerSwapReportStatus, TakerSwapReport,
};
pub use rpc::RPCConfig;
pub use spend::Destination;
pub use storage::AddressType;
pub(crate) use swapcoin::{
    IncomingSwapCoin, OutgoingSwapCoin, SwapCoin, WalletSwapCoin, WatchOnlySwapCoin,
};
pub(crate) use swapcoin2::{IncomingSwapCoinV2, OutgoingSwapCoinV2};

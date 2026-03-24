//! The Coinswap Wallet (unsecured). Used by both the Taker and Maker.

mod api;
mod backup;
mod error;
pub mod ffi;
mod fidelity;
mod funding;
mod report;
mod rpc;
mod spend;
mod split_utxos;
mod storage;
pub(crate) mod swapcoin;

pub use api::{Balances, RecoveryOutcome, UTXOSpendInfo, Wallet};
pub use backup::WalletBackup;
pub use error::WalletError;
pub use fidelity::FidelityBond;
pub(crate) use fidelity::{
    verify_fidelity_checks, FidelityError, MAX_FIDELITY_TIMELOCK, MIN_FIDELITY_TIMELOCK,
};
pub use report::{MakerFeeInfo, SwapReport, SwapRole, SwapStatus};
pub use rpc::RPCConfig;
pub use spend::Destination;
pub use storage::AddressType;

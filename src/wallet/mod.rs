//! The Coinswap Wallet (unsecured). Used by both the Taker and Maker.

mod api;
mod backup;
pub mod deniability;
mod electrum;
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

pub(crate) use api::{contract_and_timelock_vsize, infer_address_type, SpendKind};
pub use api::{Balances, RecoveryOutcome, UTXOSpendInfo, Wallet};
pub use backup::WalletBackup;
pub use deniability::{verify_deniability, DeniabilityProof, DeniabilityProofData};
pub use electrum::{ElectrumBackend, ElectrumConfig};
pub use error::WalletError;
pub use fidelity::FidelityBond;
pub(crate) use fidelity::{
    verify_fidelity_checks, FidelityError, MAX_FIDELITY_TIMELOCK, MIN_FIDELITY_TIMELOCK,
};
pub use report::{
    MakerFeeInfo, MakerReport, RecoveryReport, SwapReportFile, SwapRole, SwapStatus, TakerReport,
};
pub use rpc::{BackendConfig, BitcoindBackend, BlockchainBackend, RPCConfig};
pub use spend::Destination;
pub use storage::AddressType;

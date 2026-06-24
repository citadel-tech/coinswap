//! The Coinswap Wallet (unsecured). Used by both the Taker and Maker.

mod api;
mod backup;
pub mod deniability;
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
pub use deniability::{verify_proof, DeniabilityProof, DeniabilityProofData, ProofDirection};
pub use error::WalletError;
pub use fidelity::FidelityBond;
pub(crate) use fidelity::{
    verify_fidelity_checks, FidelityError, MAX_FIDELITY_TIMELOCK, MIN_FIDELITY_TIMELOCK,
};
pub(crate) use report::save_deniability_proofs_for_wallet;
pub use report::{
    MakerFeeInfo, MakerReport, RecoveryReport, SwapReportFile, SwapRole, SwapStatus, TakerReport,
};
pub use rpc::RPCConfig;
pub use spend::Destination;
pub use storage::AddressType;

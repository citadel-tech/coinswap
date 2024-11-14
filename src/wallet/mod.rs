//! Bitcoin wallet implementation for Coinswap protocol.
//!
//! Provides core wallet functionality for both takers and makers:
//! - UTXO management
//! - Transaction creation
//! - Swap contract handling
//! - Fidelity bond operations
//! - Direct bitcoin transfers
//! - Persistent storage
//!
//! Note: This is an unsecured wallet implementation.
//!
mod api;
mod direct_send;
mod error;
mod fidelity;
mod funding;
mod rpc;
mod storage;
mod swapcoin;

pub use api::{DisplayAddressType, UTXOSpendInfo, Wallet};
pub use direct_send::{Destination, SendAmount};
pub use error::WalletError;
pub use fidelity::{FidelityBond, FidelityError};
pub use rpc::RPCConfig;
pub use storage::WalletStore;
pub use swapcoin::{
    IncomingSwapCoin, OutgoingSwapCoin, SwapCoin, WalletSwapCoin, WatchOnlySwapCoin,
};

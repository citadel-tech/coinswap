mod api;
mod direct_send;
mod error;
pub mod fidelity;
mod funding;
mod rpc;
mod storage;
mod swapcoin;

pub use api::{DisplayAddressType, UTXOSpendInfo, Wallet};
pub use direct_send::{CoinToSpend, Destination, SendAmount};
pub use error::WalletError;
pub use rpc::RPCConfig;
pub use storage::WalletStore;
pub use swapcoin::{
    IncomingSwapCoin, OutgoingSwapCoin, SwapCoin, WalletSwapCoin, WatchOnlySwapCoin,
};

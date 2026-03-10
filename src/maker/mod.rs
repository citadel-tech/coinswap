//! The Coinswap Maker.
//!
//! A Maker server that acts as a swap service provider.
//! It can be run in an unix/mac system with local access to Bitcoin Core RPC.
//!
//! Maker server responds to RPC requests via `maker-cli` app, which is used as an
//! operating tool for the server.
//!
//! Default Ports:
//! 6102: Client connection for swaps.
//! 6103: RPC Connection for operations.

mod error;
mod rpc;

pub mod legacy_handlers;
mod legacy_verification;
pub mod taproot_handlers;
mod taproot_verification;

pub mod swap_tracker;
pub mod unified_api;
pub mod unified_handlers;
pub mod unified_server;

pub use error::MakerError;
pub use rpc::{RpcMsgReq, RpcMsgResp};

pub use swap_tracker::MakerSwapTracker;
#[cfg(feature = "integration-test")]
pub use unified_api::UnifiedMakerBehavior;
pub use unified_api::{UnifiedMakerServer, UnifiedMakerServerConfig};
pub use unified_handlers::{
    handle_message as unified_handle_message, MakerConfig as UnifiedHandlerConfig, SwapPhase,
    UnifiedConnectionState, UnifiedMaker as UnifiedMakerTrait,
};
pub use unified_server::start_unified_server;

pub use legacy_handlers::handle_legacy_message;
pub use taproot_handlers::handle_taproot_message;

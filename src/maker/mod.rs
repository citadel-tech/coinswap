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

mod api;
mod api2;
mod config;
mod error;
mod handlers;
mod handlers2;
mod rpc;
mod server;
mod server2;

pub mod legacy_handlers;
pub mod taproot_handlers;

pub mod unified_api;
pub mod unified_handlers;
pub mod unified_server;

pub use api::{Maker, MakerBehavior};
pub use error::MakerError;
pub use rpc::{RpcMsgReq, RpcMsgResp};
pub use server::start_maker_server;

pub use api2::Maker as TaprootMaker;
#[cfg(feature = "integration-test")]
pub use api2::MakerBehavior as TaprootMakerBehavior;
pub use server2::start_maker_server_taproot;

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

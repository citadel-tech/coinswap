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

pub use api::{Maker, MakerBehavior};
pub use error::MakerError;
pub use rpc::{RpcMsgReq, RpcMsgResp};
pub use server::start_maker_server;

// Taproot protocol exports
pub use api2::Maker as TaprootMaker;
#[cfg(feature = "integration-test")]
pub use api2::MakerBehavior as TaprootMakerBehavior;
pub use server2::start_maker_server_taproot;

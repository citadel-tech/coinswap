//! RPC protocol implementation for directory server remote management.
//!
//! Provides message types and handlers for remote procedure calls that allow:
//! - Querying known network addresses
//! - Serialization/deserialization of RPC messages
//! - Request/response message pairs

mod messages;
mod server;

pub use messages::{RpcMsgReq, RpcMsgResp};
pub use server::start_rpc_server_thread;

//! this module represents the directory server rpc client
mod messages;
mod server;

pub use messages::{RpcMsgReq, RpcMsgResp};
pub(crate) use server::start_rpc_server_thread;

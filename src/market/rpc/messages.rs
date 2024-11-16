use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Serialize, Deserialize, Debug)]
/// RPC request messages supported by the directory server.
///
/// These messages can be sent to the RPC server to pass a command.
pub enum RpcMsgReq {
    /// Returns the list of all known network addresses.
    ListAddresses,
}

/// RPC response messages returned by the directory server.
///
/// Corresponds to requests from RpcMsgReq, containing the results.
#[derive(Serialize, Deserialize, Debug)]
pub enum RpcMsgResp {
    /// Set of known network addresses in response to ListAddresses request.
    ListAddressesResp(HashSet<String>),
}

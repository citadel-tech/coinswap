use bitcoin::OutPoint;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Directory server RPC message request
#[derive(Serialize, Deserialize, Debug)]
pub enum RpcMsgReq {
    /// ListAddresses RPC message request variant
    ListAddresses,
}

/// Directory message RPC message Response
#[derive(Serialize, Deserialize, Debug)]
pub enum RpcMsgResp {
    /// ListAddressesResp RPC message response variant
    ListAddressesResp(BTreeSet<(OutPoint, String)>),
}

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Serialize, Deserialize, Debug)]
pub enum RpcMsgReq {
    ListAddresses,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum RpcMsgResp {
    ListAddressesResp(HashSet<String>),
}

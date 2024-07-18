use serde::{Deserialize, Serialize};
// use std::collections::HashSet;

#[derive(Serialize, Deserialize, Debug)]
pub enum RpcMsgReq {
   Ping,
   // ListAddresses,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum RpcMsgResp {
   Pong,
   // ListAddressesResp(HashSet<String>),
}
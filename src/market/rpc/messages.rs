use bitcoin::OutPoint;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Serialize, Deserialize, Debug)]
pub enum RpcMsgReq {
    ListAddresses,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum RpcMsgResp {
    ListAddressesResp(BTreeSet<(OutPoint, String)>),
}

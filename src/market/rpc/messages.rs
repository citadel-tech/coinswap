use bitcoind::bitcoincore_rpc::json::ListUnspentResultEntry;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub enum RpcMsgReq {
   Ping,
   ListAddresses,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum RpcMsgResp {
   Pong,
   ListAddressesResp { addresses: Vec<String> },
}
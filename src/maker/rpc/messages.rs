use bitcoind::bitcoincore_rpc::json::ListUnspentResultEntry;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub enum RpcMsgReq {
    Ping,
    SeedUtxo,
    SwapUtxo,
    ContractUtxo,
    FidelityUtxo,
    SeedBalance,
    SwapBalance,
    ContractBalance,
    FidelityBalance,
    NewAddress,
    SendToAddress {
        address: String,
        amount: u64,
        fee: u64,
    },
    GetTorAddress,
    GetDataDir,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum RpcMsgResp {
    Pong,
    SeedUtxoResp { utxos: Vec<ListUnspentResultEntry> },
    SwapUtxoResp { utxos: Vec<ListUnspentResultEntry> },
    FidelityUtxoResp { utxos: Vec<ListUnspentResultEntry> },
    ContractUtxoResp { utxos: Vec<ListUnspentResultEntry> },
    SeedBalanceResp(u64),
    SwapBalanceResp(u64),
    ContractBalanceResp(u64),
    FidelityBalanceResp(u64),
    NewAddressResp(String),
    SendToAddressResp(String),
    GetTorAddressResp(String),
    GetDataDirResp(String),
}

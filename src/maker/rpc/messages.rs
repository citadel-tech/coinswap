use bitcoind::bitcoincore_rpc::json::ListUnspentResultEntry;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub enum RpcMsgReq {
    Ping,
    SeedUtxo,
    SwapUtxo,
    ContractUtxo,
    FidleityUtxo,
    SeedBalance,
    SwapBalance,
    ContratBalance,
    FidleityBalance,
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
    FidleityBalanceResp(u64),
}

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
    GetNewAddress,
    TotalBalance,
    SyncOfferBook,
    DoCoinswap,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum RpcMsgResp {
    Pong,
    SeedUtxoResp { utxos: Vec<ListUnspentResultEntry> },
    SwapUtxoResp { utxos: Vec<ListUnspentResultEntry> },
    ContractUtxoResp { utxos: Vec<ListUnspentResultEntry> },
    FidelityResp { utxos: Vec<ListUnspentResultEntry> },
    SeedBalanceResp(u64),
    SwapBalanceResp(u64),
    ContractBalanceResp(u64),
    FidelityBalanceResp(u64),
    GetNewAddressResp(String),
    TotalBalanceResp(u64),
    SyncOfferBook,
    DoCoinswap,
}

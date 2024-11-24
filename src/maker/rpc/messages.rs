use std::{fmt::Display, path::PathBuf};

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
    Stop,
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
    GetDataDirResp(PathBuf),
    Shutdown,
}

impl Display for RpcMsgResp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pong => write!(f, "Pong"),
            Self::NewAddressResp(addr) => write!(f, "{}", addr),
            Self::SeedBalanceResp(bal) => write!(f, "{} sats", bal),
            Self::ContractBalanceResp(bal) => write!(f, "{} sats", bal),
            Self::SwapBalanceResp(bal) => write!(f, "{} sats", bal),
            Self::FidelityBalanceResp(bal) => write!(f, "{} sats", bal),
            Self::SeedUtxoResp { utxos } => write!(f, "{:?}", utxos),
            Self::SwapUtxoResp { utxos } => write!(f, "{:?}", utxos),
            Self::FidelityUtxoResp { utxos } => write!(f, "{:?}", utxos),
            Self::ContractUtxoResp { utxos } => write!(f, "{:?}", utxos),
            Self::SendToAddressResp(tx_hex) => write!(f, "{:?}", tx_hex),
            Self::GetTorAddressResp(addr) => write!(f, "{:?}", addr),
            Self::GetDataDirResp(path) => write!(f, "{:?}", path),
            Self::Shutdown => write!(f, "Shutdown Initiated"),
        }
    }
}

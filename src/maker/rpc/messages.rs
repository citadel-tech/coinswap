use std::fmt::Display;

use bitcoind::bitcoincore_rpc::json::ListUnspentResultEntry;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Enum representing RPC message requests.
///
/// These messages are used for various operations in the Maker-rpc communication.
/// Each variant corresponds to a specific action or query in the RPC protocol.
#[derive(Serialize, Deserialize, Debug)]
pub enum RpcMsgReq {
    /// Ping request to check connectivity.
    Ping,
    /// Request to fetch UTXOs in the seed pool.
    SeedUtxo,
    /// Request to fetch UTXOs in the swap pool.
    SwapUtxo,
    /// Request to fetch UTXOs in the contract pool.
    ContractUtxo,
    /// Request to fetch UTXOs in the fidelity pool.
    FidelityUtxo,
    /// Request to retrieve the total balance in the seed pool.
    SeedBalance,
    /// Request to retrieve the total balance in the swap pool.
    SwapBalance,
    /// Request to retrieve the total balance in the contract pool.
    ContractBalance,
    /// Request to retrieve the total balance in the fidelity pool.
    FidelityBalance,
    /// Request for generating a new wallet address.
    NewAddress,
    /// Request to send funds to a specific address.
    SendToAddress {
        /// The recipient's address.
        address: String,
        /// The amount to send.
        amount: u64,
        /// The transaction fee to include.
        fee: u64,
    },
    /// Request to retrieve the Tor address of the Maker.
    GetTorAddress,
    /// Request to retrieve the data directory path.
    GetDataDir,
    /// Request to stop the Maker server.
    Stop,
}

/// Enum representing RPC message responses.
///
/// These messages are sent in response to RPC requests and carry the results
/// of the corresponding actions or queries.
#[derive(Serialize, Deserialize, Debug)]
pub enum RpcMsgResp {
    /// Response to a Ping request.
    Pong,
    /// Response containing UTXOs in the seed pool.
    SeedUtxoResp {
        /// List of UTXOs in the seed pool.
        utxos: Vec<ListUnspentResultEntry>,
    },
    /// Response containing UTXOs in the swap pool.
    SwapUtxoResp {
        /// List of UTXOs in the swap pool.
        utxos: Vec<ListUnspentResultEntry>,
    },
    /// Response containing UTXOs in the fidelity pool.
    FidelityUtxoResp {
        /// List of UTXOs in the fidelity pool.
        utxos: Vec<ListUnspentResultEntry>,
    },
    /// Response containing UTXOs in the contract pool.
    ContractUtxoResp {
        /// List of UTXOs in the contract pool.
        utxos: Vec<ListUnspentResultEntry>,
    },
    /// Response containing the total balance in the seed pool.
    SeedBalanceResp(u64),
    /// Response containing the total balance in the swap pool.
    SwapBalanceResp(u64),
    /// Response containing the total balance in the contract pool.
    ContractBalanceResp(u64),
    /// Response containing the total balance in the fidelity pool.
    FidelityBalanceResp(u64),
    /// Response containing a newly generated wallet address.
    NewAddressResp(String),
    /// Response to a send-to-address request.
    SendToAddressResp(String),
    /// Response containing the Tor address of the Maker.
    GetTorAddressResp(String),
    /// Response containing the path to the data directory.
    GetDataDirResp(PathBuf),
    /// Response indicating the server has been shut down.
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
            Self::SendToAddressResp(tx_hex) => write!(f, "{}", tx_hex),
            Self::GetTorAddressResp(addr) => write!(f, "{}", addr),
            Self::GetDataDirResp(path) => write!(f, "{}", path.display()),
            Self::Shutdown => write!(f, "Shutdown Initiated"),
        }
    }
}

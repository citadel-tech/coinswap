use std::fmt::Display;

use bitcoin::Txid;
use bitcoind::bitcoincore_rpc::json::ListUnspentResultEntry;
use serde::{Deserialize, Serialize};
use serde_json::{json, to_string_pretty};
use std::path::PathBuf;

use crate::wallet::Balances;

/// Enum representing RPC message requests.
///
/// These messages are used for various operations in the Maker-rpc communication.
/// Each variant corresponds to a specific action or query in the RPC protocol.
#[derive(Serialize, Deserialize, Debug)]
pub enum RpcMsgReq {
    /// Ping request to check connectivity.
    Ping,
    /// Request to fetch all utxos in the wallet.
    Utxo,
    /// Request to fetch only swap utxos in the wallet.
    SwapUtxo,
    /// Request to fetch UTXOs in the contract pool.
    ContractUtxo,
    /// Request to fetch UTXOs in the fidelity pool.
    FidelityUtxo,
    /// Request to retrieve the total wallet balances of different categories.
    Balances,
    /// Request for generating a new wallet address.
    NewAddress,
    /// Request to send funds to a specific address.
    SendToAddress {
        /// The recipient's address.
        address: String,
        /// The amount to send.
        amount: u64,
        /// The transaction fee to include.
        feerate: f64,
    },
    /// Request to retrieve the Tor address of the Maker.
    GetTorAddress,
    /// Request to retrieve the data directory path.
    GetDataDir,
    /// Request to stop the Maker server.
    Stop,
    /// Request to list all active and past fidelity bonds.
    ListFidelity,
    /// Request to sync the internal wallet with blockchain.
    SyncWallet,
}

/// Enum representing RPC message responses.
///
/// These messages are sent in response to RPC requests and carry the results
/// of the corresponding actions or queries.
#[derive(Serialize, Deserialize, Debug)]
pub enum RpcMsgResp {
    /// Response to a Ping request.
    Pong,
    /// Response containing all spendable UTXOs
    UtxoResp {
        /// List of spendable UTXOs in the wallet.
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
    /// Response containing the total wallet balances of different categories.
    TotalBalanceResp(Balances),
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
    /// Response with the fidelity spending txid.
    FidelitySpend(Txid),
    /// Response with the internal server error.
    ServerError(String),
    /// Response listing all current and past fidelity bonds.
    ListBonds(String),
}

impl Display for RpcMsgResp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pong => write!(f, "Pong"),
            Self::NewAddressResp(addr) => write!(f, "{addr}"),
            Self::TotalBalanceResp(balances) => {
                write!(
                    f,
                    "{}",
                    to_string_pretty(&json!({
                        "regular": balances.regular.to_sat(),
                        "swap": balances.swap.to_sat(),
                        "contract": balances.contract.to_sat(),
                        "fidelity": balances.fidelity.to_sat(),
                        "spendable": balances.spendable.to_sat(),
                    }))
                    .unwrap()
                )
            }
            Self::UtxoResp { utxos } => write!(f, "{utxos:#?}"),
            Self::SwapUtxoResp { utxos } => write!(f, "{utxos:#?}"),
            Self::FidelityUtxoResp { utxos } => write!(f, "{utxos:#?}"),
            Self::ContractUtxoResp { utxos } => write!(f, "{utxos:#?}"),
            Self::SendToAddressResp(tx_hex) => write!(f, "{tx_hex}"),
            Self::GetTorAddressResp(addr) => write!(f, "{addr}"),
            Self::GetDataDirResp(path) => write!(f, "{}", path.display()),
            Self::Shutdown => write!(f, "Shutdown Initiated"),
            Self::FidelitySpend(txid) => write!(f, "{txid}"),
            Self::ServerError(e) => write!(f, "{e}"),
            Self::ListBonds(v) => write!(f, "{v}"),
        }
    }
}

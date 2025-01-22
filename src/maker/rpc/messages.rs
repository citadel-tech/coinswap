use std::{collections::HashMap, fmt::Display};

use bitcoin::{Amount, Txid};
use bitcoind::bitcoincore_rpc::json::ListUnspentResultEntry;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::wallet::FidelityBond;

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
    /// Request to retreive the total spenable balance in wallet.
    Balance,
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
    /// Request to reddem a fidelity bond for a given index.
    RedeemFidelity(u32),
    /// Request to list all active and past fidelity bonds.
    ListFidelity,
    /// Request to sync the internal wallet with blockchain.
    SyncWallet,
}

/// Represents balance of each utxo type.
pub struct Balance {
    /// Seed balance.
    pub regular: Amount,
    /// Incoming swap balance.
    pub swap: Amount,
    /// Unfinished timelock contract balance.
    pub contract: Amount,
    /// Amount locked in Fidelity bonds.
    pub fidelity: Amount,
    /// Spendable amount in wallet (seed + swap balance).
    pub spendable: Amount,
}

impl Display for Balance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Seed balance: {}, Swap balance: {}, Contract balance: {}, Fidelity balance: {}, Spendable balance: {}",
            self.regular.to_sat(), self.swap.to_sat(), self.contract.to_sat(), self.fidelity.to_sat(), self.regular.to_sat() + self.swap.to_sat()
        )
    }
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
        /// List of spndable UTXOs in the wallet.
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
    /// Response containing the total wallet balance.
    TotalBalanceResp(Balance),
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
    ListBonds(HashMap<u32, (FidelityBond, bool)>),
}

impl Display for RpcMsgResp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pong => write!(f, "Pong"),
            Self::NewAddressResp(addr) => write!(f, "{}", addr),
            Self::TotalBalanceResp(balance) => write!(f, "{}", balance),
            Self::UtxoResp { utxos } => write!(f, "{:#?}", utxos),
            Self::SwapUtxoResp { utxos } => write!(f, "{:#?}", utxos),
            Self::FidelityUtxoResp { utxos } => write!(f, "{:#?}", utxos),
            Self::ContractUtxoResp { utxos } => write!(f, "{:#?}", utxos),
            Self::SendToAddressResp(tx_hex) => write!(f, "{}", tx_hex),
            Self::GetTorAddressResp(addr) => write!(f, "{}", addr),
            Self::GetDataDirResp(path) => write!(f, "{}", path.display()),
            Self::Shutdown => write!(f, "Shutdown Initiated"),
            Self::FidelitySpend(txid) => write!(f, "{}", txid),
            Self::ServerError(e) => write!(f, "{}", e),
            Self::ListBonds(v) => write!(f, "{:#?}", v),
        }
    }
}

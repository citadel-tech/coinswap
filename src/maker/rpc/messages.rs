use bitcoind::bitcoincore_rpc::json::ListUnspentResultEntry;
use serde::{Deserialize, Serialize};

/// RPC request messages supported by the maker server.
///
/// Provides commands for:
/// - Server health checks
/// - UTXO management
/// - Balance queries
/// - Address generation
#[derive(Serialize, Deserialize, Debug)]
pub enum RpcMsgReq {
    /// Check if server is responding.
    Ping,
    /// Get UTXOs in the seed wallet.
    SeedUtxo,
    /// Get UTXOs in the swap wallet.
    SwapUtxo,
    /// Get UTXOs in contract transactions.
    ContractUtxo,
    /// Get UTXOs in fidelity bond.
    FidelityUtxo,
    /// Get seed wallet balance.
    SeedBalance,
    /// Get swap wallet balance.
    SwapBalance,
    /// Get contract balance.
    ContractBalance,
    /// Get fidelity bond balance.
    FidelityBalance,
    /// Generate a new address.
    NewAddress,
}

/// RPC response messages returned by the maker server.
///
/// Corresponds to requests from RpcMsgReq, containing query results for:
/// - Server health status
/// - UTXO lists
/// - Balance amounts
/// - Generated addresses
#[derive(Serialize, Deserialize, Debug)]
pub enum RpcMsgResp {
    /// Server is alive response.
    Pong,
    /// List of UTXOs in the seed wallet.
    SeedUtxoResp { utxos: Vec<ListUnspentResultEntry> },

    /// List of UTXOs in the swap wallet.
    SwapUtxoResp { utxos: Vec<ListUnspentResultEntry> },

    /// List of UTXOs in fidelity bond.
    FidelityUtxoResp { utxos: Vec<ListUnspentResultEntry> },

    /// List of UTXOs in contract transactions.
    ContractUtxoResp { utxos: Vec<ListUnspentResultEntry> },

    /// Total balance in satoshis in seed wallet.
    SeedBalanceResp(u64),

    /// Total balance in satoshis in swap wallet.
    SwapBalanceResp(u64),

    /// Total balance in satoshis in contracts.
    ContractBalanceResp(u64),

    /// Total balance in satoshis in fidelity bond.
    FidelityBalanceResp(u64),

    /// Newly generated address string.
    NewAddressResp(String),
}

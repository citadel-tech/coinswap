use std::fmt::Display;

use crate::utill::UTXO;
use bitcoin::Txid;
use serde::{Deserialize, Serialize};
use serde_json::{json, to_string_pretty};
use std::path::PathBuf;

use crate::wallet::Balances;

/// Enum representing RPC message requests.
///
/// These messages are used for various operations in the Maker-rpc communication.
/// Each variant corresponds to a specific action or query in the RPC protocol.
#[derive(minicbor::Encode, minicbor::Decode, Serialize, Deserialize, Debug)]
pub enum RpcMsgReq {
    /// Ping request to check connectivity.
    #[n(0)]
    Ping,
    /// Request to fetch all utxos in the wallet.
    #[n(1)]
    Utxo,
    /// Request to fetch only swap utxos in the wallet.
    #[n(2)]
    SwapUtxo,
    /// Request to fetch UTXOs in the contract pool.
    #[n(3)]
    ContractUtxo,
    /// Request to fetch UTXOs in the fidelity pool.
    #[n(4)]
    FidelityUtxo,
    /// Request to retrieve the total wallet balances of different categories.
    #[n(5)]
    Balances,
    /// Request for generating a new wallet address.
    #[n(6)]
    NewAddress,
    /// Request to send funds to a specific address.
    #[n(7)]
    SendToAddress {
        /// The recipient's address.
        #[n(0)]
        address: String,
        /// The amount to send.
        #[n(1)]
        amount: u64,
        /// The transaction fee to include.
        #[n(2)]
        feerate: f64,
    },
    /// Request to retrieve the Tor address of the Maker.
    #[n(8)]
    GetTorAddress,
    /// Request to retrieve the data directory path.
    #[n(9)]
    GetDataDir,
    /// Request to stop the Maker server.
    #[n(10)]
    Stop,
    /// Request to list all active and past fidelity bonds.
    #[n(11)]
    ListFidelity,
    /// Request to sync the internal wallet with blockchain.
    #[n(12)]
    SyncWallet,
}

/// Enum representing RPC message responses.
///
/// These messages are sent in response to RPC requests and carry the results
/// of the corresponding actions or queries.
#[derive(minicbor::Encode, minicbor::Decode, Serialize, Deserialize, Debug)]
pub enum RpcMsgResp {
    /// Response to a Ping request.
    #[n(0)]
    Pong,
    /// Response containing all spendable UTXOs
    #[n(1)]
    UtxoResp {
        /// List of spendable UTXOs in the wallet.
        #[n(0)]
        #[cbor(encode_with = "encode_json_bytes", decode_with = "decode_json_bytes")]
        utxos: Vec<UTXO>,
    },
    /// Response containing UTXOs in the swap pool.
    #[n(2)]
    SwapUtxoResp {
        /// List of UTXOs in the swap pool.
        #[n(0)]
        #[cbor(encode_with = "encode_json_bytes", decode_with = "decode_json_bytes")]
        utxos: Vec<UTXO>,
    },
    /// Response containing UTXOs in the fidelity pool.
    #[n(3)]
    FidelityUtxoResp {
        /// List of UTXOs in the fidelity pool.
        #[n(0)]
        #[cbor(encode_with = "encode_json_bytes", decode_with = "decode_json_bytes")]
        utxos: Vec<UTXO>,
    },
    /// Response containing UTXOs in the contract pool.
    #[n(4)]
    ContractUtxoResp {
        /// List of UTXOs in the contract pool.
        #[n(0)]
        #[cbor(encode_with = "encode_json_bytes", decode_with = "decode_json_bytes")]
        utxos: Vec<UTXO>,
    },
    /// Response containing the total wallet balances of different categories.
    #[n(5)]
    TotalBalanceResp(
        #[n(0)]
        #[cbor(encode_with = "encode_json_bytes", decode_with = "decode_json_bytes")]
        Balances,
    ),
    /// Response containing a newly generated wallet address.
    #[n(6)]
    NewAddressResp(#[n(0)] String),
    /// Response to a send-to-address request.
    #[n(7)]
    SendToAddressResp(#[n(0)] String),
    /// Response containing the Tor address of the Maker.
    #[n(8)]
    GetTorAddressResp(#[n(0)] String),
    /// Response containing the path to the data directory.
    #[n(9)]
    GetDataDirResp(
        #[n(0)]
        #[cbor(encode_with = "encode_path_buf", decode_with = "decode_path_buf")]
        PathBuf,
    ),
    /// Response indicating the server has been shut down.
    #[n(10)]
    Shutdown,
    /// Response with the fidelity spending txid.
    #[n(11)]
    FidelitySpend(
        #[n(0)]
        #[cbor(encode_with = "encode_json_bytes", decode_with = "decode_json_bytes")]
        Txid,
    ),
    /// Response with the internal server error.
    #[n(12)]
    ServerError(#[n(0)] String),
    /// Response listing all current and past fidelity bonds.
    #[n(13)]
    ListBonds(#[n(0)] String),
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
            Self::UtxoResp { utxos }
            | Self::SwapUtxoResp { utxos }
            | Self::FidelityUtxoResp { utxos }
            | Self::ContractUtxoResp { utxos } => {
                write!(
                    f,
                    "{}",
                    serde_json::to_string_pretty(utxos).expect("UTXO JSON serialization failed")
                )
            }
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

// Inline minicbor helpers
#[allow(dead_code)]
fn encode_json_bytes<T: serde::Serialize, W: minicbor::encode::Write, C>(
    x: &T,
    e: &mut minicbor::Encoder<W>,
    _ctx: &mut C,
) -> Result<(), minicbor::encode::Error<W::Error>> {
    e.bytes(&serde_json::to_vec(x).map_err(|_| minicbor::encode::Error::message("json error"))?)?;
    Ok(())
}
fn decode_json_bytes<T: serde::de::DeserializeOwned, C>(
    d: &mut minicbor::Decoder<'_>,
    _ctx: &mut C,
) -> Result<T, minicbor::decode::Error> {
    serde_json::from_slice(d.bytes()?)
        .map_err(|_| minicbor::decode::Error::message("json decode error"))
}

fn encode_path_buf<W: minicbor::encode::Write, C>(
    x: &std::path::PathBuf,
    e: &mut minicbor::Encoder<W>,
    _ctx: &mut C,
) -> Result<(), minicbor::encode::Error<W::Error>> {
    e.encode(x.to_str().unwrap_or(""))?;
    Ok(())
}
fn decode_path_buf<C>(
    d: &mut minicbor::Decoder<'_>,
    _ctx: &mut C,
) -> Result<std::path::PathBuf, minicbor::decode::Error> {
    Ok(std::path::PathBuf::from(d.decode::<String>()?))
}

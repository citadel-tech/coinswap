//! This module defines the messages communicated between the parties(Taker, Maker and DNS)

// Taker | Message | Maker
// --GetOffer-->
// <--Offer--
// --TxId(ProofOfContract), Hashlock Script, Timelock Script, PartialPubkey_A ||
// PartialPubkey_C -->
// <-- Hashlock Script, Timelock Script, PartialPubkey_D, TxId(ProofOfTransaction)--
// --PartialSignature_A-->
// <--PartialSignature_D--
// Coinswap Successful

// **The taker does not need to propagate the Hash Preimage**

use bitcoin::{PublicKey, ScriptBuf, Txid, Transaction};
use secp256k1::musig::{PublicNonce, PartialSignature};
use crate::wallet::FidelityBond;
use bitcoin::hashes::sha256::Hash;

struct FidelityProof {
    pub(crate) bond: FidelityBond,
    pub(crate) cert_hash: Hash,
    pub(crate) cert_sig: bitcoin::secp256k1::ecdsa::Signature,
}

enum TakerToMakerMessage {
    GetOffer,
    SwapDetails,
    SendersContract,
    PartialSignatureAndNonce
}

enum MakerToTakerMessage {
    Offer,
    AckResponse,
    ReceiversContract,
    PartialSignatureAndNonce
}

// Taker to Maker
struct GetOffer {
    protocol_version_min: u32,
    protocol_version_max: u32,
    number_of_transactions: u32,
}

pub struct ContractInfo {
    contract_tx: Transaction,
    hashlock_script: ScriptBuf,
    timelock_script: ScriptBuf,
    sender_nonce: Option<PublicNonce>,
    receiver_nonce: Option<PublicNonce>
}

// Maker to Taker
struct Offer {
    tweakable_point: PublicKey,
    public_nonces: Vec<PublicNonce>,
    base_fee: u64,
    amount_relative_fee: f64,
    time_relative_fee: f64,
    minimum_locktime: u16,
    fidelity: FidelityProof,
    min_size: u64,
    max_size: u64,
}

// Taker to Maker
pub struct SwapDetails{

}

// Maker to Taker
pub enum AckResponse {
    Ack,
    Nack
}

// TakerToMakerMessage
struct SendersContract {
    contract_txs: Vec<Txid>,
    // Below data is used to verify transaction
    pubkeys_a: Vec<PublicKey>,
    hashlock_scripts: Vec<ScriptBuf>,
    timelock_scripts: Vec<ScriptBuf>,
    // Tweakable point for allowing maker to create next contract
    next_party_tweakable_point: PublicKey,
    // Below nonce allows the maker to generate partial sigs
    next_party_pub_nonces: Vec<PublicNonce>
}

struct ReceiverToSenderContract {
    contract_txs: Vec<Txid>,
    // Below data is used to verify transaction
    pubkeys_b: Vec<PublicKey>,
    hashlock_scripts: Vec<ScriptBuf>,
    timelock_scripts: Vec<ScriptBuf>,
}

struct PartialSignaturesAndNonces {
    partial_signatures: Vec<PartialSignature>,
    pub_nonces: Vec<PublicNonce>,
}

enum DnsResponse {
    Ack,
    Nack(String),
}

struct DnsMetadata {
    url: String,
    proof: FidelityProof,
}

enum DnsRequest {
    Post {
        metadata: DnsMetadata,
    },
    Get,
}
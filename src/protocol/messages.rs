//we make heavy use of serde_json's de/serialization for the benefits of
//having the compiler check for us, extra type checking and readability

//this works because of enum representations in serde
//see https://serde.rs/enum-representations.html

//! Coinswap Protocol Messages.
//!
//! Messages are Communicated between Taker and one or many Makers.
//! Makers don't communicate with each other. One Maker will only know the Identity of the Maker, in previous and next hop.
//!
//! Messages are named in  terms of `Sender` and `Receiver` as identification of their context.  They refer to sender and receiver sides of each hop.
//! A party (Taker/Maker) will act as both Sender and Receiver in one coinswap hop.
//!
//! `Sender`: When the party is sending coins into the coinswap multisig. They will have the Sender side of the HTLC
//! and respond to sender specific messages.
//!
//! `Receiver`: When the party is receiving coins from the coinswap multisig. They will have the Receiver side of the
//! HTLC and respond to receiver specific messages.
//!
//! The simplest 3 hop Coinswap communication, between a Taker and two Makers in a multi-hop coinswap is shown below.
//!
//! Taker -----> Maker1 -----> Maker2 ------> Taker
//!
//! ********* Initiate First Hop *********
//! (Sender: Taker, Receiver: Maker1)
//! Taker -> Maker1: [TakerToMakerMessage::ReqContractSigsForSender]
//! Maker1 -> Taker: [MakerToTakerMessage::RespContractSigsForSender]
//! Taker -> Maker1: [TakerToMakerMessage::RespProofOfFunding] (Funding Tx of the hop Taker-Maker1)
//!
//! ********* Initiate Second Hop *********
//! Taker -> Maker1: Share details of next hop. (Sender: Maker1, Receiver: Maker2)
//! Maker1 -> Taker: [MakerToTakerMessage::ReqContractSigsAsRecvrAndSender]
//! Taker -> Maker2: [`TakerToMakerMessage::ReqContractSigsForSender`] (Request the Receiver for it's sigs)
//! Maker2 -> Taker: [MakerToTakerMessage::RespContractSigsForSender] (Receiver sends the sigs)
//! Taker puts his sigs as the Sender.
//! Taker -> Maker1: [TakerToMakerMessage::RespContractSigsForRecvrAndSender] (send both the sigs)
//! Maker1 Broadcasts the funding transaction.
//! Taker -> Maker2: [TakerToMakerMessage::RespProofOfFunding] (Funding Tx of swap Maker1-Maker2)
//!
//! ********* Initiate Third Hop *********
//! Taker -> Maker2: Shares details of next hop. (Sender: Maker2, Receiver: Taker)
//! Maker2 -> Taker: [MakerToTakerMessage::ReqContractSigsAsRecvrAndSender]
//! Taker -> Maker1: [TakerToMakerMessage::ReqContractSigsForRecvr] (Request the Sender for it's sigs)
//! Maker1 -> Taker: [MakerToTakerMessage::RespContractSigsForRecvr] (Sender sends the the sigs)
//! Taker puts his sigs as the Receiver.
//! Taker -> Maker2: [TakerToMakerMessage::RespContractSigsForRecvrAndSender]
//! Broadcast Maker2-Taker Funding Transaction.
//! Taker -> Maker2: [TakerToMakerMessage::ReqContractSigsForRecvr]
//! Maker2 -> Taker: [MakerToTakerMessage::RespContractSigsForRecvr]
//! Maker2 Broadcasts the funding transaction.
//!
//! ********* Settlement *********
//! Taker -> Maker1: [TakerToMakerMessage::RespHashPreimage] (For Taker-Maker1 HTLC)
//! Maker1 -> Taker: [MakerToTakerMessage::RespPrivKeyHandover] (For Maker1-Maker2 funding multisig).
//! Taker -> Maker1: [TakerToMakerMessage::RespPrivKeyHandover] (For Taker-Maker1 funding multisig).
//! Taker -> Maker2:  [TakerToMakerMessage::RespHashPreimage] (for Maker1-Maker2 HTLC).
//! Taker -> Maker2: [TakerToMakerMessage::RespPrivKeyHandover] (For Maker1-Maker2 funding multisig, received from Maker1 in Step 16)
//! Taker -> Maker2: [`TakerToMakerMessage::RespHashPreimage`] (for Maker2-Taker HTLC).
//! Maker2 -> Taker: [`MakerToTakerMessage::RespPrivKeyHandover`] (For Maker2-Taker funding multisig).

use bitcoin::{
    secp256k1::{SecretKey, Signature},
    OutPoint, PublicKey, Script, Transaction,
};

use serde::{Deserialize, Serialize};

use bitcoin::hashes::hash160::Hash as Hash160;

pub const PREIMAGE_LEN: usize = 32;
pub type Preimage = [u8; PREIMAGE_LEN];

#[derive(Debug, Serialize, Deserialize)]
pub struct TakerHello {
    pub protocol_version_min: u32,
    pub protocol_version_max: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GiveOffer;

/// Contract Sigs requesting information for the Sender side of the hop.
#[derive(Debug, Serialize, Deserialize)]
pub struct ContractTxInfoForSender {
    pub multisig_key_nonce: SecretKey,
    pub hashlock_key_nonce: SecretKey,
    pub timelock_pubkey: PublicKey,
    pub senders_contract_tx: Transaction,
    pub multisig_redeemscript: Script,
    pub funding_input_value: u64,
}

/// Request for Contract Sigs **for** the Sender side of the hop.
#[derive(Debug, Serialize, Deserialize)]
pub struct ReqContractSigsForSender {
    pub txs_info: Vec<ContractTxInfoForSender>,
    pub hashvalue: Hash160,
    pub locktime: u16,
}

/// Contract Sigs requesting information for the Receiver side of the hop.
#[derive(Debug, Serialize, Deserialize)]
pub struct ContractTxInfoForRecvr {
    pub multisig_redeemscript: Script,
    pub contract_tx: Transaction,
}

/// Request for Contract Sigs **for** the Receiver side of the hop.
#[derive(Debug, Serialize, Deserialize)]
pub struct ReqContractSigsForRecvr {
    pub txs: Vec<ContractTxInfoForRecvr>,
}

/// Confirmed Funding Tx with extra metadata.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FundingTxInfo {
    pub funding_tx: Transaction,
    pub funding_tx_merkleproof: String,
    pub multisig_redeemscript: Script,
    pub multisig_nonce: SecretKey,
    pub contract_redeemscript: Script,
    pub hashlock_nonce: SecretKey,
}

/// PublickKey information for the next hop of Coinswap.
#[derive(Debug, Serialize, Deserialize)]
pub struct NextHopInfo {
    pub next_multisig_pubkey: PublicKey,
    pub next_hashlock_pubkey: PublicKey,
}

/// Message sent to the Coinswap Receiver that funding transaction has been confirmed.
/// Including information for the next hop of the coinswap.
#[derive(Debug, Serialize, Deserialize)]
pub struct ProofOfFunding {
    pub confirmed_funding_txes: Vec<FundingTxInfo>,
    // TODO: Directly use Vec of Pubkeys.
    pub next_coinswap_info: Vec<NextHopInfo>,
    pub next_locktime: u16,
    pub next_fee_rate: u64,
}

/// Signatures required for an intermediate Maker to perform receiving and sending of coinswaps.
/// These are signatures from the peer of this Maker.
///
/// For Ex: A coinswap hop sequence as Maker1 ----> Maker2 -----> Maker3.
/// This message from Maker2 will contain the signatures as below:
/// `receivers_sigs`: Signatures from Maker1. Maker1 is Sender, and Maker2 is Receiver.
/// `senders_sigs`: Signatures from Maker3. Maker3 is Receiver and Maker2 is Sender.
#[derive(Debug, Serialize, Deserialize)]
pub struct ContractSigsForRecvrAndSender {
    /// Sigs from previous peer for Contract Tx of previous hop, (coinswap received by this Maker).
    pub receivers_sigs: Vec<Signature>,
    /// Sigs from the next peer for Contract Tx of next hop, (coinswap sent by this Maker).
    pub senders_sigs: Vec<Signature>,
}

/// Message to Transfer `HashPreimage` from Taker to Makers.
#[derive(Debug, Serialize, Deserialize)]
pub struct HashPreimage {
    pub senders_multisig_redeemscripts: Vec<Script>,
    pub receivers_multisig_redeemscripts: Vec<Script>,
    pub preimage: [u8; 32],
}

/// Multisig Privatekeys used in the last step of coinswap to perform privatekey handover.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MultisigPrivkey {
    pub multisig_redeemscript: Script,
    pub key: SecretKey,
}

/// Message to perform the final Privatekey Handover. This is the last message of the Coinswap Protocol.
#[derive(Debug, Serialize, Deserialize)]
pub struct PrivKeyHandover {
    pub multisig_privkeys: Vec<MultisigPrivkey>,
}

/// All messages sent from Taker to Maker.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "lowercase")]
pub enum TakerToMakerMessage {
    /// Protocol Handshake.
    TakerHello(TakerHello),
    /// Request the Maker's Offer advertisement.
    ReqGiveOffer(GiveOffer),
    /// Request Contract Sigs **for** the Sender side of the hop. The Maker receiving this message is the Receiver of the hop.
    ReqContractSigsForSender(ReqContractSigsForSender),
    /// Respond with the [ProofOfFunding] message. This is sent when the funding transaction gets confirmed.
    RespProofOfFunding(ProofOfFunding),
    /// Request Contract Sigs **for** the Receiver and Sender side of the Hop.
    RespContractSigsForRecvrAndSender(ContractSigsForRecvrAndSender),
    /// Request Contract Sigs **for** the Receiver side of the hop. The Maker receiving this message is the Sender of the hop.
    ReqContractSigsForRecvr(ReqContractSigsForRecvr),
    /// Respond with the hash preimage. This settles the HTLC contract. The Receiver side will use this preimage unlock the HTLC.
    RespHashPreimage(HashPreimage),
    /// Respond by handing over the Private Keys of coinswap multisig. This denotes the completion of the whole swap.
    RespPrivKeyHandover(PrivKeyHandover),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MakerHello {
    pub protocol_version_min: u32,
    pub protocol_version_max: u32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FidelityBondProof {
    pub utxo: OutPoint,
    pub utxo_key: PublicKey,
    pub locktime: i64,
    pub cert_sig: Signature,
    pub cert_expiry: u16,
    pub cert_pubkey: PublicKey,
    pub onion_sig: Signature,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Offer {
    pub absolute_fee_sat: u64,
    pub amount_relative_fee_ppb: u64,
    pub time_relative_fee_ppb: u64,
    pub required_confirms: i32,
    pub minimum_locktime: u16,
    pub max_size: u64,
    pub min_size: u64,
    pub tweakable_point: PublicKey,
}

/// Contract Tx signatures provided by a Sender of a Coinswap.
#[derive(Debug, Serialize, Deserialize)]
pub struct ContractSigsForSender {
    pub sigs: Vec<Signature>,
}

/// Contract Tx and extra metadata from a Sender of a Coinswap
#[derive(Debug, Serialize, Deserialize)]
pub struct SenderContractTxInfo {
    pub contract_tx: Transaction,
    pub timelock_pubkey: PublicKey,
    pub multisig_redeemscript: Script,
    pub funding_amount: u64,
}

/// This message is sent by a Maker to a Taker. Which is a request to the Taker for gathering signatures for the Maker as both Sender and Receiver of Coinswaps.
/// This message is sent by a Maker after a [`ProofOfFunding`] has been received.
#[derive(Debug, Serialize, Deserialize)]
pub struct ContractSigsAsRecvrAndSender {
    /// Contract Tx by which this maker is receiving Coinswap.
    pub receivers_contract_txs: Vec<Transaction>,
    /// Contract Tx info by which this maker is sending Coinswap.
    pub senders_contract_txs_info: Vec<SenderContractTxInfo>,
}

/// Contract Tx signatures a Maker sends as a Receiver of coinswap.
#[derive(Debug, Serialize, Deserialize)]
pub struct ContractSigsForRecvr {
    pub sigs: Vec<Signature>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "lowercase")]
pub enum MakerToTakerMessage {
    /// Protocol Handshake.
    MakerHello(MakerHello),
    /// Send the Maker's offer advertisement.
    RespOffer(Offer),
    /// Send Contract Sigs **for** the Sender side of the hop. The Maker sending this message is the Receiver of the hop.
    RespContractSigsForSender(ContractSigsForSender),
    /// Request Contract Sigs, **as** both the Sending and Receiving side of the hop.
    ReqContractSigsAsRecvrAndSender(ContractSigsAsRecvrAndSender),
    /// Send Contract Sigs **for** the Receiver side of the hop. The Maker sending this message is the Sender of the hop.
    RespContractSigsForRecvr(ContractSigsForRecvr),
    /// Send the multisig private keys of the swap, declaring completion of the contract.
    RespPrivKeyHandover(PrivKeyHandover),
}

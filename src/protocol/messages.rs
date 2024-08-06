//! Coinswap Protocol Messages.
//!
//! Messages are communicated between one Taker and one or many Makers.
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
//! ```shell
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
//! ```

use std::fmt::Display;

use bitcoin::{
    ecdsa::Signature, hashes::sha256d::Hash, secp256k1::SecretKey, Amount, PublicKey, ScriptBuf,
    Transaction,
};

use serde::{Deserialize, Serialize};

use bitcoin::hashes::hash160::Hash as Hash160;

use crate::wallet::FidelityBond;

/// Defines the length of the Preimage.
pub const PREIMAGE_LEN: usize = 32;

/// Type for Preimage.
pub type Preimage = [u8; PREIMAGE_LEN];

/// Represents the initial handshake message sent from Taker to Maker.
#[derive(Debug, Serialize, Deserialize)]
pub struct TakerHello {
    pub protocol_version_min: u32,
    pub protocol_version_max: u32,
}

/// Represents a request to give an offer.
#[derive(Debug, Serialize, Deserialize)]
pub struct GiveOffer;

/// Contract Sigs requesting information for the Sender side of the hop.
#[derive(Debug, Serialize, Deserialize)]
pub struct ContractTxInfoForSender {
    pub multisig_nonce: SecretKey,
    pub hashlock_nonce: SecretKey,
    pub timelock_pubkey: PublicKey,
    pub senders_contract_tx: Transaction,
    pub multisig_redeemscript: ScriptBuf,
    pub funding_input_value: Amount,
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
    pub multisig_redeemscript: ScriptBuf,
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
    pub multisig_redeemscript: ScriptBuf,
    pub multisig_nonce: SecretKey,
    pub contract_redeemscript: ScriptBuf,
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

/// Message to Transfer [`HashPreimage`] from Taker to Makers.
#[derive(Debug, Serialize, Deserialize)]
pub struct HashPreimage {
    pub senders_multisig_redeemscripts: Vec<ScriptBuf>,
    pub receivers_multisig_redeemscripts: Vec<ScriptBuf>,
    pub preimage: [u8; 32],
}

/// Multisig Privatekeys used in the last step of coinswap to perform privatekey handover.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MultisigPrivkey {
    pub multisig_redeemscript: ScriptBuf,
    pub key: SecretKey,
}

/// Message to perform the final Privatekey Handover. This is the last message of the Coinswap Protocol.
#[derive(Debug, Serialize, Deserialize)]
pub struct PrivKeyHandover {
    pub multisig_privkeys: Vec<MultisigPrivkey>,
}

/// All messages sent from Taker to Maker.
#[derive(Debug, Serialize, Deserialize)]
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

impl Display for TakerToMakerMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TakerHello(_) => write!(f, "TakerHello"),
            Self::ReqGiveOffer(_) => write!(f, "ReqGiveOffer"),
            Self::ReqContractSigsForSender(_) => write!(f, "ReqContractSigsForSender"),
            Self::RespProofOfFunding(_) => write!(f, "RespProofOfFunding"),
            Self::RespContractSigsForRecvrAndSender(_) => {
                write!(f, "RespContractSigsForRecvrAndSender")
            }
            Self::ReqContractSigsForRecvr(_) => write!(f, "ReqContractSigsForRecvr"),
            Self::RespHashPreimage(_) => write!(f, "RespHashPreimage"),
            Self::RespPrivKeyHandover(_) => write!(f, "RespPrivKeyHandover"),
        }
    }
}

/// Represents the initial handshake message sent from Maker to Taker.
#[derive(Debug, Serialize, Deserialize)]
pub struct MakerHello {
    pub protocol_version_min: u32,
    pub protocol_version_max: u32,
}

/// Contains proof data related to fidelity bond.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Hash)]
pub struct FidelityProof {
    pub bond: FidelityBond,
    pub cert_hash: Hash,
    pub cert_sig: bitcoin::secp256k1::ecdsa::Signature,
}

/// Represents an offer in the context of the Coinswap protocol.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, PartialOrd, Hash)]
pub struct Offer {
    pub absolute_fee_sat: Amount,
    pub amount_relative_fee_ppb: Amount,
    pub time_relative_fee_ppb: Amount,
    pub required_confirms: u64,
    pub minimum_locktime: u16,
    pub max_size: u64,
    pub min_size: u64,
    pub tweakable_point: PublicKey,
    pub fidelity: FidelityProof,
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
    pub multisig_redeemscript: ScriptBuf,
    pub funding_amount: Amount,
}

/// This message is sent by a Maker to a Taker, which is a request to the Taker for gathering signatures for the Maker as both Sender and Receiver of Coinswaps.
/// This message is sent by a Maker after a [`ProofOfFunding`] has been received.
#[derive(Debug, Serialize, Deserialize)]
pub struct ContractSigsAsRecvrAndSender {
    /// Contract Tx by which this maker is receiving Coinswap.
    pub receivers_contract_txs: Vec<Transaction>,
    /// Contract Tx info by which this maker is sending Coinswap.
    pub senders_contract_txs_info: Vec<SenderContractTxInfo>,
}

/// Contract Tx signatures a Maker sends as a Receiver of CoinSwap.
#[derive(Debug, Serialize, Deserialize)]
pub struct ContractSigsForRecvr {
    pub sigs: Vec<Signature>,
}

/// All messages sent from Maker to Taker.
#[derive(Debug, Serialize, Deserialize)]
pub enum MakerToTakerMessage {
    /// Protocol Handshake.
    MakerHello(MakerHello),
    /// Send the Maker's offer advertisement.
    RespOffer(Box<Offer>), // Add box as Offer has large size due to fidelity bond
    /// Send Contract Sigs **for** the Sender side of the hop. The Maker sending this message is the Receiver of the hop.
    RespContractSigsForSender(ContractSigsForSender),
    /// Request Contract Sigs, **as** both the Sending and Receiving side of the hop.
    ReqContractSigsAsRecvrAndSender(ContractSigsAsRecvrAndSender),
    /// Send Contract Sigs **for** the Receiver side of the hop. The Maker sending this message is the Sender of the hop.
    RespContractSigsForRecvr(ContractSigsForRecvr),
    /// Send the multisig private keys of the swap, declaring completion of the contract.
    RespPrivKeyHandover(PrivKeyHandover),
}

impl Display for MakerToTakerMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MakerHello(_) => write!(f, "MakerHello"),
            Self::RespOffer(_) => write!(f, "RespOffer"),
            Self::RespContractSigsForSender(_) => write!(f, "RespContractSigsForSender"),
            Self::ReqContractSigsAsRecvrAndSender(_) => {
                write!(f, "ReqContractSigsAsRecvrAndSender")
            }
            Self::RespContractSigsForRecvr(_) => {
                write!(f, "RespContractSigsForRecvr")
            }
            Self::RespPrivKeyHandover(_) => write!(f, "RespPrivKeyHandover"),
        }
    }
}

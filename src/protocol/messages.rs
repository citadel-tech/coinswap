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

/// Initial handshake message sent from taker to establish protocol version.
///
/// Contains:
/// - Minimum supported version
/// - Maximum supported version
#[derive(Debug, Serialize, Deserialize)]
pub struct TakerHello {
    /// Minimum protocol version supported by taker.
    pub protocol_version_min: u32,
    /// Maximum protocol version supported by taker.
    pub protocol_version_max: u32,
}

/// Represents a request to give an offer.
#[derive(Debug, Serialize, Deserialize)]
pub struct GiveOffer;

/// Contract details needed to generate sender signatures.
///
/// Contains:
/// - Nonces for key derivation
/// - Contract parameters
/// - Transaction details
#[derive(Debug, Serialize, Deserialize)]
pub struct ContractTxInfoForSender {
    /// Nonce for multisig key derivation.
    pub multisig_nonce: SecretKey,
    /// Nonce for hashlock key derivation.
    pub hashlock_nonce: SecretKey,
    /// Public key for timelock condition.
    pub timelock_pubkey: PublicKey,
    /// Contract transaction requiring signatures.
    pub senders_contract_tx: Transaction,
    /// Script for multisig validation.
    pub multisig_redeemscript: ScriptBuf,
    /// Amount in the funding input.
    pub funding_input_value: Amount,
}
/// Request for contract signatures from sender's maker.
///
/// Contains:
/// - Contract transactions needing signatures
/// - Hash for contract conditions
/// - Timelock value
#[derive(Debug, Serialize, Deserialize)]
pub struct ReqContractSigsForSender {
    /// Contract details for signature generation.
    pub txs_info: Vec<ContractTxInfoForSender>,
    /// Hash value for contract condition.
    pub hashvalue: Hash160,
    /// Timelock value for contract.
    pub locktime: u16,
}

/// Contract details needed to generate receiver signatures.
///
/// Contains:
/// - Multisig script for signing
/// - Contract transaction to sign
#[derive(Debug, Serialize, Deserialize)]
pub struct ContractTxInfoForRecvr {
    /// Script for multisig validation.
    pub multisig_redeemscript: ScriptBuf,
    /// Transaction requiring signatures.
    pub contract_tx: Transaction,
}

/// Request for contract signatures from receiver's maker.
///
/// Contains:
/// - Contract transactions needing signatures
/// - Associated multisig scripts
#[derive(Debug, Serialize, Deserialize)]
pub struct ReqContractSigsForRecvr {
    /// Contract details for signature generation.
    pub txs: Vec<ContractTxInfoForRecvr>,
}

/// Confirmed funding transaction with verification and contract data.
///
/// Contains:
/// - Transaction details
/// - Merkle proof of inclusion
/// - Scripts and nonces for contracts
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FundingTxInfo {
    /// The funding transaction.
    pub funding_tx: Transaction,
    /// Merkle proof of transaction confirmation.
    pub funding_tx_merkleproof: String,
    /// Script for multisig validation.
    pub multisig_redeemscript: ScriptBuf,
    /// Nonce for multisig key derivation.
    pub multisig_nonce: SecretKey,
    /// Script defining contract conditions.
    pub contract_redeemscript: ScriptBuf,
    /// Nonce for hashlock key derivation.
    pub hashlock_nonce: SecretKey,
}

/// Public keys needed for setting up the next swap hop.
///
/// Contains:
/// - Multisig public key
/// - Hashlock public key
#[derive(Debug, Serialize, Deserialize)]
pub struct NextHopInfo {
    /// Public key for next hop's multisig.
    pub next_multisig_pubkey: PublicKey,
    /// Public key for next hop's hashlock.
    pub next_hashlock_pubkey: PublicKey,
}
/// Funding confirmation message with next hop parameters.
///
/// Contains:
/// - Confirmed funding transactions
/// - Next hop public keys
/// - Next contract parameters
// TODO: Directly use Vec of Pubkeys
#[derive(Debug, Serialize, Deserialize)]
pub struct ProofOfFunding {
    /// List of confirmed funding transactions with proofs.
    pub confirmed_funding_txes: Vec<FundingTxInfo>,
    /// Public keys for next swap hops.
    pub next_coinswap_info: Vec<NextHopInfo>,
    /// Timelock value for next contracts.
    pub next_locktime: u16,
    /// Fee rate for next transactions.
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

/// Hash preimage reveal message sent from taker to makers.
///
/// Contains:
/// - Multisig scripts for both parties
/// - 32-byte preimage for contract settlement
#[derive(Debug, Serialize, Deserialize)]
pub struct HashPreimage {
    /// Multisig scripts for sending makers.
    pub senders_multisig_redeemscripts: Vec<ScriptBuf>,
    /// Multisig scripts for receiving makers.
    pub receivers_multisig_redeemscripts: Vec<ScriptBuf>,
    /// Preimage that hashes to contract hashlock value.
    pub preimage: [u8; 32],
}

/// Private key and script for final coinswap handover step.
///
/// Contains:
/// - Multisig script for verification
/// - Private key for the multisig
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct MultisigPrivkey {
    /// Script identifying the multisig.
    pub multisig_redeemscript: ScriptBuf,
    /// Private key for the multisig.
    pub key: SecretKey,
}

/// Final coinswap message containing private keys for swap completion.
///
/// Contains:
/// - List of multisig private keys
/// - Marks end of swap protocol
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrivKeyHandover {
    /// Private keys for all involved multisigs.
    pub multisig_privkeys: Vec<MultisigPrivkey>,
}

/// All messages sent from Taker to Maker during swap protocol.
///
/// Handles communication for:
/// - Protocol initialization
/// - Offer requests
/// - Contract signatures
/// - Funding proofs
/// - Contract settlement
#[derive(Debug, Serialize, Deserialize)]
pub enum TakerToMakerMessage {
    /// Initial protocol handshake message.
    TakerHello(TakerHello),
    /// Request for maker's current offer.
    ReqGiveOffer(GiveOffer),
    /// Request contract signatures from receiving maker for sender.
    ReqContractSigsForSender(ReqContractSigsForSender),
    /// Confirmation that funding transaction is confirmed.
    RespProofOfFunding(ProofOfFunding),
    /// Contract signatures when maker acts as both receiver and sender.
    RespContractSigsForRecvrAndSender(ContractSigsForRecvrAndSender),
    /// Request contract signatures from sending maker for receiver.
    ReqContractSigsForRecvr(ReqContractSigsForRecvr),
    /// Hash preimage to settle HTLC contract.
    RespHashPreimage(HashPreimage),
    /// Private key handover indicating swap completion.
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

/// Initial handshake message sent from maker to establish protocol version.
///
/// Contains:
/// - Minimum supported version
/// - Maximum supported version
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct MakerHello {
    /// Minimum protocol version supported by maker.
    pub protocol_version_min: u32,
    /// Maximum protocol version supported by maker.
    pub protocol_version_max: u32,
}

/// Proof of fidelity bond ownership and validity.
///
/// Contains:
/// - Bond details
/// - Certificate hash
/// - Signature proving ownership
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Hash)]
pub struct FidelityProof {
    /// Details of the fidelity bond.
    pub bond: FidelityBond,
    /// Hash of the bond certificate.
    pub cert_hash: Hash,
    /// Signature proving bond ownership.
    pub cert_sig: bitcoin::secp256k1::ecdsa::Signature,
}

/// Maker's offer parameters for participating in coinswap.
///
/// Contains:
/// - Fee structure (absolute and relative)
/// - Time and size constraints
/// - Key derivation base
/// - Fidelity bond proof
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, PartialOrd, Hash)]
pub struct Offer {
    /// Base fee in satoshis.
    pub absolute_fee_sat: Amount,
    /// Fee relative to swap amount in parts per billion.
    pub amount_relative_fee_ppb: Amount,
    /// Fee relative to timelock in parts per billion per block.
    pub time_relative_fee_ppb: Amount,
    /// Required confirmations for funding transaction.
    pub required_confirms: u64,
    /// Minimum timelock period for contracts.
    pub minimum_locktime: u16,
    /// Maximum swap amount accepted.
    pub max_size: u64,
    /// Minimum swap amount accepted.
    pub min_size: u64,
    /// Base point for key derivation.
    pub tweakable_point: PublicKey,
    /// Proof of maker's fidelity bond.
    pub fidelity: FidelityProof,
}

/// Contract transaction signatures from maker acting as swap sender.
///
/// Contains signatures for:
/// - Contract transaction inputs
/// - Sent to taker for verification
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContractSigsForSender {
    /// Signatures for contract transaction.
    pub sigs: Vec<Signature>,
}

/// Contract transaction details from sending party in coinswap.
///
/// Contains:
/// - Contract transaction
/// - Keys and scripts
/// - Funding information
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SenderContractTxInfo {
    /// Contract transaction to be signed.
    pub contract_tx: Transaction,
    /// Public key for timelock condition.
    pub timelock_pubkey: PublicKey,
    /// Script for multisig validation.
    pub multisig_redeemscript: ScriptBuf,
    /// Amount being committed to contract.
    pub funding_amount: Amount,
}

/// This message is sent by a Maker to a Taker, which is a request to the Taker for gathering signatures
/// for the Maker as both Sender and Receiver of Coinswaps.
///
/// This message is sent by a Maker after a [`ProofOfFunding`] has been received.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContractSigsAsRecvrAndSender {
    /// Contract Tx by which this maker is receiving Coinswap.
    pub receivers_contract_txs: Vec<Transaction>,
    /// Contract Tx info by which this maker is sending Coinswap.
    pub senders_contract_txs_info: Vec<SenderContractTxInfo>,
}

/// Contract transaction signatures from maker acting as swap receiver.
///
/// Contains signatures for:
/// - Contract transaction inputs
/// - Sent to taker for verification
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContractSigsForRecvr {
    /// Signatures for contract transaction.
    pub sigs: Vec<Signature>,
}

/// All messages sent from Maker to Taker during swap protocol.
///
/// Handles communication for:
/// - Protocol initialization
/// - Offer advertisement
/// - Contract signatures
/// - Key exchange
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum MakerToTakerMessage {
    /// Initial protocol handshake message.
    MakerHello(MakerHello),
    /// Maker's swap offer details and bond.
    RespOffer(Box<Offer>), // Boxed due to large size from fidelity bond
    /// Contract signatures for sender from receiving maker.
    RespContractSigsForSender(ContractSigsForSender),
    /// Contract signature request when maker is both sender and receiver.
    ReqContractSigsAsRecvrAndSender(ContractSigsAsRecvrAndSender),
    /// Contract signatures for receiver from sending maker.
    RespContractSigsForRecvr(ContractSigsForRecvr),
    /// Private key handover indicating contract completion.
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

//we make heavy use of serde_json's de/serialization for the benefits of
//having the compiler check for us, extra type checking and readability

//this works because of enum representations in serde
//see https://serde.rs/enum-representations.html

//! This module describes the Coinswap Protocol Messages.
//! Messages are Communicated between Taker and Individual Makers.
//! Makers don't communicate with each other and only know the Identity of the Maker in previous and next hop.
//!
//! The detailed steps of the communication protocol between Taker and Makers in a multi-hop coinswap is as below.
//! At each hop a Maker acts as both Sender and Receiver of Coinswap. The `Send` and `Receive` keywords are used to
//! distinguish between different message types.
//!
//! Taker -----> Maker1 -----> Maker2 ------> Taker
//!
//! 1. Taker -> Maker1: [`TakerToMakerMessage::ReqContractSigsForSender`]
//! 2. Maker1 -> Taker: [`MakerToTakerMessage::ContractSigsForSender`]
//! Taker got the signature for the contract, so he broadcasts the funding txs.
//! 3A. Taker -> Maker1: [`TakerToMakerMessage::ProofOfFunding`] Funding Tx of Swap Taker-Maker1.
//! 3B. Taker -> Maker1: Share details of Maker2.
//! In next step Maker1 asks Taker signatures for both the contract txs,
//! Taker's signature for Taker-Maker1 contract, and Maker2's signature for Maker1-Maker2 contract.
//! 4. Maker1 -> Taker: [`MakerToTakerMessage::RequestContractSigsAsReceiverAndSender`]
//! 5A. Taker -> Maker2: [`TakerToMakerMessage::ReqContractSigsForSender`]
//! 5B: Maker2 -> Taker: [`MakerToTakerMessage::ContractSigsForSender`]
//! 5B: Taker Signs Taker-Maker1 Contract as Sender.
//! 5C: Taker -> Maker1: [`TakerToMakerMessage::ContractSigsForRecvingAndSending`]
//! 6. Maker1 Broadcasts the funding transaction.
//! 7. Taker -> Maker2: [`TakerToMakerMessage::ProofOfFunding`] Funding Tx of Swap Maker1-Maker2
//! 8. Taker -> Maker2: Shares details of next hop, Taker's receiving Pubkey in this case.
//! 9. Maker2 -> Taker: [`MakerToTakerMessage::RequestContractSigsAsReceiverAndSender`]
//! 10. Taker -> Maker1: [`TakerToMakerMessage::ReqContractSigsForRecvr`]
//! 10. Maker1 -> Taker: [`MakerToTakerMessage::ContractSigsForRecvr]
//! 11. Taker Signs Maker2-Taker Contract as Receiver.
//! 12. Taker -> Maker2: [`TakerToMakerMessage::ContractSigsForRecvingAndSending`]
//! 13. Broadcast Maker2-Taker Funding Transaction.
//! xx. Taker -> Maker2: [`TakerToMakerMessage::ReqContractSigsForRecvr]
//! 14. Maker2 -> Taker: [`MakerToTakerMessage::ContractSigsForRecvr`]
//!
//! This Completes the Coinswap Round. Next is the Hash Preimage and Privatekey Handover.
//! 15. Taker -> Maker1: [`TakerToMakerMessage::HashPreimage`] For Taker-Maker1 Contract.
//! 16. Maker1 -> Taker: [`MakerToTakerMessage::PrivateKeyHandover`] For Privkey of Maker1 between Maker1-Maker2 swap.
//! 17. Taker -> Maker1: [`TakerToMakerMessage::PrivateKeyHandover`] For Privkey of Taker between Taker-Maker1 Swap.
//! 18. Taker -> Maker2:  [`TakerToMakerMessage::HashPreimage`] for Maker1-Maker2 Contract.
//! 19. Taker -> Maker2: [`TakerToMakerMessage::PrivateKeyHandover`] as got in Step 16.
//! 20. Taker -> Maker2: [`TakerToMakerMessage::HashPreimage`] for Maker2-Taker Contract.
//! 21. Maker2 -> Taker: [`MakerToTakerMessage::PrivateKeyHandover`] for Privkey of Maker2 between Maker2-Taker swap.

use bitcoin::{OutPoint, Script, Transaction};

#[derive(Debug, Serialize, Deserialize)]
pub struct TakerHello {
    pub protocol_version_min: u32,
    pub protocol_version_max: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GiveOffer;

/// Contract Transaction with extra metadata for a Sender.
#[derive(Debug, Serialize, Deserialize)]
pub struct ContractTxForSender {
    pub multisig_key_nonce: SecretKey,
    pub hashlock_key_nonce: SecretKey,
    pub timelock_pubkey: PublicKey,
    pub senders_contract_tx: Transaction,
    pub multisig_redeemscript: Script,
    pub funding_input_value: u64,
}

/// A message to request Contract Tx signatures for a Sender.
#[derive(Debug, Serialize, Deserialize)]
pub struct ReqContractSigsForSender {
    pub txs_info: Vec<ContractTxForSender>,
    pub hashvalue: Hash160,
    pub locktime: u16,
}

/// Confirmed Funding Tx with extra metadata.
#[derive(Debug, Serialize, Deserialize)]
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
/// This message for Maker2 will contain the signatures for Contract Tx as below:
/// `receivers_sigs`: Signatures from Maker1.
/// `senders_sigs`: Signatures from Maker3
#[derive(Debug, Serialize, Deserialize)]
pub struct ContractSigsForRecvingAndSending {
    /// Sigs from previous peer for Contract Tx of previous hop, (coinswap received by this Maker).
    pub receivers_sigs: Vec<Signature>,
    /// Sigs from the next peer for Contract Tx of next hop, (coinswap sent by this Maker).
    pub senders_sigs: Vec<Signature>,
}

/// Contract Tx with multisig reedemscript for a Coinswap Receiver.
#[derive(Debug, Serialize, Deserialize)]
pub struct ContractTxForRecvr {
    pub multisig_redeemscript: Script,
    pub contract_tx: Transaction,
}

/// Message to request Contract Tx signatures for a Receiver of a Coinswap.
#[derive(Debug, Serialize, Deserialize)]
pub struct ReqContractSigsForRecvr {
    pub txs: Vec<ContractTxForRecvr>,
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
pub struct PrivateKeyHandover {
    pub multisig_privkeys: Vec<MultisigPrivkey>,
}

/// All messages sent from Taker to Maker.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "lowercase")]
pub enum TakerToMakerMessage {
    TakerHello(TakerHello),
    GiveOffer(GiveOffer),
    /// Request for Contract Tx Sigs for Sender of a Swap.
    ReqContractSigsForSender(ReqContractSigsForSender),
    ProofOfFunding(ProofOfFunding),
    /// Contract Tx Sigs to Maker acting as both Sender and Receiver in a hop.
    ContractSigsForRecvingAndSending(ContractSigsForRecvingAndSending),
    /// Request for Contract Tx Sigs for Receiver of a Swap.
    ReqContractSigsForRecvr(ReqContractSigsForRecvr),
    HashPreimage(HashPreimage),
    PrivateKeyHandover(PrivateKeyHandover),
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

#[derive(Debug, Serialize, Deserialize, Clone)]
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

/// This message is sent by a Maker to a Taker. Which is a request to gather signatures for the Maker as both Sender and Receiver of Coinswaps.
/// This message is sent by a Maker after a [`ProofOfFunding`] has been received.
#[derive(Debug, Serialize, Deserialize)]
pub struct RequestContractSigsAsReceiverAndSender {
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
    MakerHello(MakerHello),
    Offer(Offer),
    /// Contract Tx Sigs when the Maker is acting as a Sender.
    ContractSigsForSender(ContractSigsForSender),
    /// Request for Contract Tx Sigs for a Maker for both Sending and Receiving Side of a Swap.
    RequestContractSigsAsReceiverAndSender(RequestContractSigsAsReceiverAndSender),
    /// Contract Tx Sigs when a Maker is Acting as a Reciever.
    ContractSigsForRecvr(ContractSigsForRecvr),
    PrivateKeyHandover(PrivateKeyHandover),
}

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
pub(crate) const PREIMAGE_LEN: usize = 32;

/// Type for Preimage.
pub(crate) type Preimage = [u8; PREIMAGE_LEN];

/// Represents the initial handshake message sent from Taker to Maker.
#[derive(minicbor::Encode, minicbor::Decode, Debug, Serialize, Deserialize)]
pub(crate) struct TakerHello {
    #[n(0)]
    pub(crate) protocol_version_min: u32,
    #[n(1)]
    pub(crate) protocol_version_max: u32,
}

/// Represents a request to give an offer.
#[derive(minicbor::Encode, minicbor::Decode, Debug, Serialize, Deserialize)]
pub(crate) struct GiveOffer;

/// Contract Sigs requesting information for the Sender side of the hop.
#[derive(minicbor::Encode, minicbor::Decode, Debug, Serialize, Deserialize)]
pub(crate) struct ContractTxInfoForSender {
    #[n(0)]
    #[cbor(encode_with = "encode_secret_key", decode_with = "decode_secret_key")]
    pub(crate) multisig_nonce: SecretKey,
    #[n(1)]
    #[cbor(encode_with = "encode_secret_key", decode_with = "decode_secret_key")]
    pub(crate) hashlock_nonce: SecretKey,
    #[n(2)]
    #[cbor(encode_with = "encode_public_key", decode_with = "decode_public_key")]
    pub(crate) timelock_pubkey: PublicKey,
    #[n(3)]
    #[cbor(encode_with = "encode_transaction", decode_with = "decode_transaction")]
    pub(crate) senders_contract_tx: Transaction,
    #[n(4)]
    #[cbor(encode_with = "encode_script_buf", decode_with = "decode_script_buf")]
    pub(crate) multisig_redeemscript: ScriptBuf,
    #[n(5)]
    #[cbor(encode_with = "encode_amount", decode_with = "decode_amount")]
    pub(crate) funding_input_value: Amount,
}

/// Request for Contract Sigs **for** the Sender side of the hop.
#[derive(minicbor::Encode, minicbor::Decode, Debug, Serialize, Deserialize)]
pub(crate) struct ReqContractSigsForSender {
    #[n(0)]
    pub(crate) txs_info: Vec<ContractTxInfoForSender>,
    #[n(1)]
    #[cbor(
        encode_with = "encode_hash160_hash",
        decode_with = "decode_hash160_hash"
    )]
    pub(crate) hashvalue: Hash160,
    #[n(2)]
    pub(crate) locktime: u16,
}

/// Contract Sigs requesting information for the Receiver side of the hop.
#[derive(minicbor::Encode, minicbor::Decode, Debug, Serialize, Deserialize)]
pub(crate) struct ContractTxInfoForRecvr {
    #[n(0)]
    #[cbor(encode_with = "encode_script_buf", decode_with = "decode_script_buf")]
    pub(crate) multisig_redeemscript: ScriptBuf,
    #[n(1)]
    #[cbor(encode_with = "encode_transaction", decode_with = "decode_transaction")]
    pub(crate) contract_tx: Transaction,
}

/// Request for Contract Sigs **for** the Receiver side of the hop.
#[derive(minicbor::Encode, minicbor::Decode, Debug, Serialize, Deserialize)]
pub(crate) struct ReqContractSigsForRecvr {
    #[n(0)]
    pub(crate) txs: Vec<ContractTxInfoForRecvr>,
}

/// Confirmed Funding Tx with extra metadata.
#[derive(minicbor::Encode, minicbor::Decode, Debug, Serialize, Deserialize, Clone)]
pub(crate) struct FundingTxInfo {
    #[n(0)]
    #[cbor(encode_with = "encode_transaction", decode_with = "decode_transaction")]
    pub(crate) funding_tx: Transaction,
    #[n(1)]
    pub(crate) funding_tx_merkleproof: String,
    #[n(2)]
    #[cbor(encode_with = "encode_script_buf", decode_with = "decode_script_buf")]
    pub(crate) multisig_redeemscript: ScriptBuf,
    #[n(3)]
    #[cbor(encode_with = "encode_secret_key", decode_with = "decode_secret_key")]
    pub(crate) multisig_nonce: SecretKey,
    #[n(4)]
    #[cbor(encode_with = "encode_script_buf", decode_with = "decode_script_buf")]
    pub(crate) contract_redeemscript: ScriptBuf,
    #[n(5)]
    #[cbor(encode_with = "encode_secret_key", decode_with = "decode_secret_key")]
    pub(crate) hashlock_nonce: SecretKey,
}

/// PublicKey information for the next hop of Coinswap.
#[derive(minicbor::Encode, minicbor::Decode, Debug, Serialize, Deserialize)]
pub(crate) struct NextHopInfo {
    #[n(0)]
    #[cbor(encode_with = "encode_public_key", decode_with = "decode_public_key")]
    pub(crate) next_multisig_pubkey: PublicKey,
    #[n(1)]
    #[cbor(encode_with = "encode_public_key", decode_with = "decode_public_key")]
    pub(crate) next_hashlock_pubkey: PublicKey,
}

/// Message sent to the Coinswap Receiver that funding transaction has been confirmed.
/// Including information for the next hop of the coinswap.
#[derive(minicbor::Encode, minicbor::Decode, Debug, Serialize, Deserialize)]
pub(crate) struct ProofOfFunding {
    #[n(0)]
    pub(crate) confirmed_funding_txes: Vec<FundingTxInfo>,
    #[n(1)]
    pub(crate) next_coinswap_info: Vec<NextHopInfo>,
    #[n(2)]
    pub(crate) refund_locktime: u16,
    #[n(3)]
    pub(crate) contract_feerate: f64,
    #[n(4)]
    pub(crate) id: String,
}

/// Signatures required for an intermediate Maker to perform receiving and sending of coinswaps.
/// These are signatures from the peer of this Maker.
///
/// For Ex: A coinswap hop sequence as Maker1 ----> Maker2 -----> Maker3.
/// This message from Maker2 will contain the signatures as below:
/// `receivers_sigs`: Signatures from Maker1. Maker1 is Sender, and Maker2 is Receiver.
/// `senders_sigs`: Signatures from Maker3. Maker3 is Receiver and Maker2 is Sender.
#[derive(minicbor::Encode, minicbor::Decode, Debug, Serialize, Deserialize)]
pub(crate) struct ContractSigsForRecvrAndSender {
    /// Sigs from previous peer for Contract Tx of previous hop, (coinswap received by this Maker).
    #[n(0)]
    #[cbor(
        encode_with = "encode_vec_bitcoin_signature",
        decode_with = "decode_vec_bitcoin_signature"
    )]
    pub(crate) receivers_sigs: Vec<Signature>,
    /// Sigs from the next peer for Contract Tx of next hop, (coinswap sent by this Maker).
    #[n(1)]
    #[cbor(
        encode_with = "encode_vec_bitcoin_signature",
        decode_with = "decode_vec_bitcoin_signature"
    )]
    pub(crate) senders_sigs: Vec<Signature>,
    /// Unique ID for a swap
    #[n(2)]
    pub(crate) id: String,
}

/// Message to Transfer [`HashPreimage`] from Taker to Makers.
#[derive(minicbor::Encode, minicbor::Decode, Debug, Serialize, Deserialize)]
pub(crate) struct HashPreimage {
    #[n(0)]
    #[cbor(
        encode_with = "encode_vec_script_buf",
        decode_with = "decode_vec_script_buf"
    )]
    pub(crate) senders_multisig_redeemscripts: Vec<ScriptBuf>,
    #[n(1)]
    #[cbor(
        encode_with = "encode_vec_script_buf",
        decode_with = "decode_vec_script_buf"
    )]
    pub(crate) receivers_multisig_redeemscripts: Vec<ScriptBuf>,
    #[n(2)]
    pub(crate) preimage: [u8; 32],
}

/// Multisig Privatekeys used in the last step of coinswap to perform privatekey handover.
#[derive(minicbor::Encode, minicbor::Decode, Debug, Serialize, Deserialize, Clone)]
pub(crate) struct MultisigPrivkey {
    #[n(0)]
    #[cbor(encode_with = "encode_script_buf", decode_with = "decode_script_buf")]
    pub(crate) multisig_redeemscript: ScriptBuf,
    #[n(1)]
    #[cbor(encode_with = "encode_secret_key", decode_with = "decode_secret_key")]
    pub(crate) key: SecretKey,
}

/// Message to perform the final Privatekey Handover. This is the last message of the Coinswap Protocol.
#[derive(minicbor::Encode, minicbor::Decode, Debug, Serialize, Deserialize)]
pub(crate) struct PrivKeyHandover {
    #[n(0)]
    pub(crate) multisig_privkeys: Vec<MultisigPrivkey>,
    /// Unique ID to remove the connection state for a completed swap so watchtowers are not triggered.
    #[n(1)]
    pub(crate) id: String,
}

/// All messages sent from Taker to Maker.
#[derive(minicbor::Encode, minicbor::Decode, Debug, Serialize, Deserialize)]
pub(crate) enum TakerToMakerMessage {
    /// Protocol Handshake.
    #[n(0)]
    TakerHello(#[n(0)] TakerHello),
    /// Request the Maker's Offer advertisement.
    #[n(1)]
    ReqGiveOffer(#[n(0)] GiveOffer),
    /// Request Contract Sigs **for** the Sender side of the hop. The Maker receiving this message is the Receiver of the hop.
    #[n(2)]
    ReqContractSigsForSender(#[n(0)] ReqContractSigsForSender),
    /// Respond with the [`ProofOfFunding`] message. This is sent when the funding transaction gets confirmed.
    #[n(3)]
    RespProofOfFunding(#[n(0)] ProofOfFunding),
    /// Respond with Contract Sigs **for** the Receiver and Sender side of the Hop.
    #[n(4)]
    RespContractSigsForRecvrAndSender(#[n(0)] ContractSigsForRecvrAndSender),
    /// Request Contract Sigs **for** the Receiver side of the hop. The Maker receiving this message is the Sender of the hop.
    #[n(5)]
    ReqContractSigsForRecvr(#[n(0)] ReqContractSigsForRecvr),
    /// Respond with the hash preimage. This settles the HTLC contract. The Receiver side will use this preimage unlock the HTLC.
    #[n(6)]
    RespHashPreimage(#[n(0)] HashPreimage),
    /// Respond by handing over the Private Keys of coinswap multisig. This denotes the completion of the whole swap.
    #[n(7)]
    RespPrivKeyHandover(#[n(0)] PrivKeyHandover),
    #[n(8)]
    WaitingFundingConfirmation(#[n(0)] String),
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
            Self::WaitingFundingConfirmation(_) => write!(f, "WaitingFundingConfirmation"),
        }
    }
}

/// Represents the initial handshake message sent from Maker to Taker.
#[derive(minicbor::Encode, minicbor::Decode, Debug, Serialize, Deserialize)]
pub(crate) struct MakerHello {
    #[n(0)]
    pub(crate) protocol_version_min: u32,
    #[n(1)]
    pub(crate) protocol_version_max: u32,
}

/// Contains proof data related to fidelity bond.
#[derive(minicbor::Encode, minicbor::Decode, Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FidelityProof {
    /// Details for Fidelity Bond
    #[n(0)]
    pub bond: FidelityBond,
    /// Double SHA256 hash of certificate message proving bond ownership and binding to maker address
    #[n(1)]
    #[cbor(
        encode_with = "encode_sha256d_hash",
        decode_with = "decode_sha256d_hash"
    )]
    pub cert_hash: Hash,
    /// ECDSA signature over cert_hash using the bond's private key
    #[n(2)]
    #[cbor(encode_with = "encode_signature", decode_with = "decode_signature")]
    pub cert_sig: bitcoin::secp256k1::ecdsa::Signature,
}

/// Represents an offer in the context of the Coinswap protocol.
#[derive(minicbor::Encode, minicbor::Decode, Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct Offer {
    /// Base fee charged per swap in satoshis (fixed cost component)
    #[n(0)]
    pub base_fee: u64, // base fee in sats
    /// Percentage fee relative to swap amount
    #[n(1)]
    pub amount_relative_fee_pct: f64, // % fee on total amount
    /// Percentage fee for time-locked funds
    #[n(2)]
    pub time_relative_fee_pct: f64, // amount * refund_locktime * TRF% = fees for locking the fund.
    /// Minimum confirmations required before proceeding with swap
    #[n(3)]
    pub required_confirms: u32,
    /// Minimum timelock duration in blocks for contract transactions
    #[n(4)]
    pub minimum_locktime: u16,
    /// Maximum swap amount accepted in sats
    #[n(5)]
    pub max_size: u64,
    /// Minimum swap amount accepted in sats
    #[n(6)]
    pub min_size: u64,
    /// Displayed public key of makers, for receiving swaps.
    /// Actual swap addresses are derived from this public key using unique nonces per swap.
    #[n(7)]
    #[cbor(encode_with = "encode_public_key", decode_with = "decode_public_key")]
    pub tweakable_point: PublicKey,
    /// Cryptographic proof of fidelity bond for Sybil resistance
    #[n(8)]
    pub fidelity: FidelityProof,
}

/// Contract Tx signatures provided by a Sender of a Coinswap.
#[derive(minicbor::Encode, minicbor::Decode, Debug, Serialize, Deserialize)]
pub(crate) struct ContractSigsForSender {
    #[n(0)]
    #[cbor(
        encode_with = "encode_vec_bitcoin_signature",
        decode_with = "decode_vec_bitcoin_signature"
    )]
    pub(crate) sigs: Vec<Signature>,
}

/// Contract Tx and extra metadata from a Sender of a Coinswap
#[derive(minicbor::Encode, minicbor::Decode, Debug, Serialize, Deserialize)]
pub(crate) struct SenderContractTxInfo {
    #[n(0)]
    #[cbor(encode_with = "encode_transaction", decode_with = "decode_transaction")]
    pub(crate) contract_tx: Transaction,
    #[n(1)]
    #[cbor(encode_with = "encode_public_key", decode_with = "decode_public_key")]
    pub(crate) timelock_pubkey: PublicKey,
    #[n(2)]
    #[cbor(encode_with = "encode_script_buf", decode_with = "decode_script_buf")]
    pub(crate) multisig_redeemscript: ScriptBuf,
    #[n(3)]
    #[cbor(encode_with = "encode_amount", decode_with = "decode_amount")]
    pub(crate) funding_amount: Amount,
}

/// This message is sent by a Maker to a Taker, which is a request to the Taker for gathering signatures
/// for the Maker as both Sender and Receiver of Coinswaps.
///
/// This message is sent by a Maker after a [`ProofOfFunding`] has been received.
#[derive(minicbor::Encode, minicbor::Decode, Debug, Serialize, Deserialize)]
pub(crate) struct ContractSigsAsRecvrAndSender {
    /// Contract Tx by which this maker is receiving Coinswap.
    #[n(0)]
    #[cbor(
        encode_with = "encode_vec_transaction",
        decode_with = "decode_vec_transaction"
    )]
    pub(crate) receivers_contract_txs: Vec<Transaction>,
    /// Contract Tx info by which this maker is sending Coinswap.
    #[n(1)]
    pub(crate) senders_contract_txs_info: Vec<SenderContractTxInfo>,
}

/// Contract Tx signatures a Maker sends as a Receiver of CoinSwap.
#[derive(minicbor::Encode, minicbor::Decode, Debug, Serialize, Deserialize)]
pub(crate) struct ContractSigsForRecvr {
    #[n(0)]
    #[cbor(
        encode_with = "encode_vec_bitcoin_signature",
        decode_with = "decode_vec_bitcoin_signature"
    )]
    pub(crate) sigs: Vec<Signature>,
}

/// All messages sent from Maker to Taker.
#[derive(minicbor::Encode, minicbor::Decode, Debug, Serialize, Deserialize)]
pub(crate) enum MakerToTakerMessage {
    /// Protocol Handshake.
    #[n(0)]
    MakerHello(#[n(0)] MakerHello),
    /// Send the Maker's offer advertisement.
    #[n(1)]
    RespOffer(#[n(0)] Box<Offer>), // Add box as Offer has large size due to fidelity bond
    /// Send Contract Sigs **for** the Sender side of the hop. The Maker sending this message is the Receiver of the hop.
    #[n(2)]
    RespContractSigsForSender(#[n(0)] ContractSigsForSender),
    /// Request Contract Sigs, **as** both the Sending and Receiving side of the hop.
    #[n(3)]
    ReqContractSigsAsRecvrAndSender(#[n(0)] ContractSigsAsRecvrAndSender),
    /// Send Contract Sigs **for** the Receiver side of the hop. The Maker sending this message is the Sender of the hop.
    #[n(4)]
    RespContractSigsForRecvr(#[n(0)] ContractSigsForRecvr),
    /// Send the multisig private keys of the swap, declaring completion of the contract.
    #[n(5)]
    RespPrivKeyHandover(#[n(0)] PrivKeyHandover),
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

// Inline minicbor helpers
#[allow(dead_code)]
fn encode_transaction<W: minicbor::encode::Write, C>(
    x: &bitcoin::Transaction,
    e: &mut minicbor::Encoder<W>,
    _ctx: &mut C,
) -> Result<(), minicbor::encode::Error<W::Error>> {
    e.bytes(&bitcoin::consensus::serialize(x))?;
    Ok(())
}
fn decode_transaction<C>(
    d: &mut minicbor::Decoder<'_>,
    _ctx: &mut C,
) -> Result<bitcoin::Transaction, minicbor::decode::Error> {
    bitcoin::consensus::deserialize(d.bytes()?)
        .map_err(|_| minicbor::decode::Error::message("invalid transaction"))
}

fn encode_amount<W: minicbor::encode::Write, C>(
    x: &bitcoin::Amount,
    e: &mut minicbor::Encoder<W>,
    _ctx: &mut C,
) -> Result<(), minicbor::encode::Error<W::Error>> {
    e.u64(x.to_sat())?;
    Ok(())
}
fn decode_amount<C>(
    d: &mut minicbor::Decoder<'_>,
    _ctx: &mut C,
) -> Result<bitcoin::Amount, minicbor::decode::Error> {
    Ok(bitcoin::Amount::from_sat(d.u64()?))
}

fn encode_hash160_hash<W: minicbor::encode::Write, C>(
    x: &bitcoin::hashes::hash160::Hash,
    e: &mut minicbor::Encoder<W>,
    _ctx: &mut C,
) -> Result<(), minicbor::encode::Error<W::Error>> {
    e.bytes(&x[..])?;
    Ok(())
}
fn decode_hash160_hash<C>(
    d: &mut minicbor::Decoder<'_>,
    _ctx: &mut C,
) -> Result<bitcoin::hashes::hash160::Hash, minicbor::decode::Error> {
    use bitcoin::hashes::Hash;
    bitcoin::hashes::hash160::Hash::from_slice(d.bytes()?)
        .map_err(|_| minicbor::decode::Error::message("invalid hash"))
}

fn encode_public_key<W: minicbor::encode::Write, C>(
    x: &bitcoin::PublicKey,
    e: &mut minicbor::Encoder<W>,
    _ctx: &mut C,
) -> Result<(), minicbor::encode::Error<W::Error>> {
    e.encode(x.to_string())?;
    Ok(())
}
fn decode_public_key<C>(
    d: &mut minicbor::Decoder<'_>,
    _ctx: &mut C,
) -> Result<bitcoin::PublicKey, minicbor::decode::Error> {
    let s = d.decode::<String>()?;
    std::str::FromStr::from_str(&s)
        .map_err(|_| minicbor::decode::Error::message("invalid public key"))
}

fn encode_vec_bitcoin_signature<W: minicbor::encode::Write, C>(
    x: &Vec<bitcoin::ecdsa::Signature>,
    e: &mut minicbor::Encoder<W>,
    _ctx: &mut C,
) -> Result<(), minicbor::encode::Error<W::Error>> {
    e.array(x.len() as u64)?;
    for item in x {
        e.bytes(&item.serialize())?;
    }
    Ok(())
}
fn decode_vec_bitcoin_signature<C>(
    d: &mut minicbor::Decoder<'_>,
    _ctx: &mut C,
) -> Result<Vec<bitcoin::ecdsa::Signature>, minicbor::decode::Error> {
    let n = d.array()?.unwrap_or(0) as usize;
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        v.push(
            bitcoin::ecdsa::Signature::from_slice(d.bytes()?)
                .map_err(|_| minicbor::decode::Error::message("invalid sig"))?,
        );
    }
    Ok(v)
}

fn encode_secret_key<W: minicbor::encode::Write, C>(
    x: &bitcoin::secp256k1::SecretKey,
    e: &mut minicbor::Encoder<W>,
    _ctx: &mut C,
) -> Result<(), minicbor::encode::Error<W::Error>> {
    e.bytes(&x.secret_bytes())?;
    Ok(())
}
fn decode_secret_key<C>(
    d: &mut minicbor::Decoder<'_>,
    _ctx: &mut C,
) -> Result<bitcoin::secp256k1::SecretKey, minicbor::decode::Error> {
    bitcoin::secp256k1::SecretKey::from_slice(d.bytes()?)
        .map_err(|_| minicbor::decode::Error::message("invalid secret key"))
}

fn encode_vec_transaction<W: minicbor::encode::Write, C>(
    x: &Vec<bitcoin::Transaction>,
    e: &mut minicbor::Encoder<W>,
    _ctx: &mut C,
) -> Result<(), minicbor::encode::Error<W::Error>> {
    e.array(x.len() as u64)?;
    for item in x {
        e.bytes(&bitcoin::consensus::serialize(item))?;
    }
    Ok(())
}
fn decode_vec_transaction<C>(
    d: &mut minicbor::Decoder<'_>,
    _ctx: &mut C,
) -> Result<Vec<bitcoin::Transaction>, minicbor::decode::Error> {
    let n = d.array()?.unwrap_or(0) as usize;
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        v.push(
            bitcoin::consensus::deserialize(d.bytes()?)
                .map_err(|_| minicbor::decode::Error::message("invalid tx"))?,
        );
    }
    Ok(v)
}

fn encode_sha256d_hash<W: minicbor::encode::Write, C>(
    x: &bitcoin::hashes::sha256d::Hash,
    e: &mut minicbor::Encoder<W>,
    _ctx: &mut C,
) -> Result<(), minicbor::encode::Error<W::Error>> {
    e.bytes(&x[..])?;
    Ok(())
}
fn decode_sha256d_hash<C>(
    d: &mut minicbor::Decoder<'_>,
    _ctx: &mut C,
) -> Result<bitcoin::hashes::sha256d::Hash, minicbor::decode::Error> {
    use bitcoin::hashes::Hash;
    bitcoin::hashes::sha256d::Hash::from_slice(d.bytes()?)
        .map_err(|_| minicbor::decode::Error::message("invalid hash"))
}

fn encode_signature<W: minicbor::encode::Write, C>(
    x: &bitcoin::secp256k1::ecdsa::Signature,
    e: &mut minicbor::Encoder<W>,
    _ctx: &mut C,
) -> Result<(), minicbor::encode::Error<W::Error>> {
    e.bytes(&x.serialize_der())?;
    Ok(())
}
fn decode_signature<C>(
    d: &mut minicbor::Decoder<'_>,
    _ctx: &mut C,
) -> Result<bitcoin::secp256k1::ecdsa::Signature, minicbor::decode::Error> {
    bitcoin::secp256k1::ecdsa::Signature::from_der(d.bytes()?)
        .map_err(|_| minicbor::decode::Error::message("invalid signature"))
}

fn encode_script_buf<W: minicbor::encode::Write, C>(
    x: &bitcoin::ScriptBuf,
    e: &mut minicbor::Encoder<W>,
    _ctx: &mut C,
) -> Result<(), minicbor::encode::Error<W::Error>> {
    e.bytes(x.as_bytes())?;
    Ok(())
}
fn decode_script_buf<C>(
    d: &mut minicbor::Decoder<'_>,
    _ctx: &mut C,
) -> Result<bitcoin::ScriptBuf, minicbor::decode::Error> {
    Ok(bitcoin::ScriptBuf::from(d.bytes()?.to_vec()))
}

fn encode_vec_script_buf<W: minicbor::encode::Write, C>(
    x: &Vec<bitcoin::ScriptBuf>,
    e: &mut minicbor::Encoder<W>,
    _ctx: &mut C,
) -> Result<(), minicbor::encode::Error<W::Error>> {
    e.array(x.len() as u64)?;
    for item in x {
        e.bytes(item.as_bytes())?;
    }
    Ok(())
}
fn decode_vec_script_buf<C>(
    d: &mut minicbor::Decoder<'_>,
    _ctx: &mut C,
) -> Result<Vec<bitcoin::ScriptBuf>, minicbor::decode::Error> {
    let n = d.array()?.unwrap_or(0) as usize;
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        v.push(bitcoin::ScriptBuf::from(d.bytes()?.to_vec()));
    }
    Ok(v)
}

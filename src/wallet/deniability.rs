//! Deniability proof data, generation, and verification.
//!
//! A proof lets a wallet show control of a swap contract without revealing
//! private keys.

use std::fmt;

use bitcoin::{
    consensus::encode::serialize,
    ecdsa,
    hashes::{sha256, Hash},
    secp256k1::{self, ecdsa::Signature as SecpEcdsaSignature, Keypair, Secp256k1, SecretKey},
    OutPoint, PublicKey, ScriptBuf, Transaction, TxOut, XOnlyPublicKey,
};
use serde::{Deserialize, Serialize};

use crate::{
    protocol::{
        common_messages::ProtocolVersion,
        contract::{read_hashlock_pubkey_from_contract, read_pubkeys_from_multisig_redeemscript},
        contract2::create_taproot_script,
        musig_interface::get_aggregated_pubkey_compat,
    },
    utill::redeemscript_to_scriptpubkey,
};

use super::{report::SwapRole, swapcoin::IncomingSwapCoin, WalletError};

/// Direction of the swapcoin this proof describes from this wallet's perspective.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProofDirection {
    /// Contract paid to this wallet.
    Incoming,
    /// Contract paid away from this wallet.
    Outgoing,
    /// Contract is an intermediate hop observed by this wallet.
    WatchOnly,
}

/// Top-level deniability proof persisted by the wallet.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeniabilityProof {
    /// Unique proof id, stable across reloads.
    pub proof_id: String,
    /// Swap id this proof belongs to.
    pub swap_id: String,
    /// Participant role.
    pub role: SwapRole,
    /// Swapcoin direction.
    pub direction: ProofDirection,
    /// Protocol version.
    pub protocol: ProtocolVersion,
    /// Matching outgoing contract/funding outpoint when known.
    pub outgoing_swapcoin: Option<OutPoint>,
    /// Protocol-specific proof payload.
    pub proof: DeniabilityProofData,
    /// Unix timestamp when the proof was created.
    pub created_at: u64,
}

/// Protocol-specific proof payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DeniabilityProofData {
    /// Taproot/MuSig2 proof.
    Taproot(TaprootDeniabilityProof),
    /// Legacy P2WSH proof.
    Legacy(LegacyDeniabilityProof),
}

/// Taproot deniability proof.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaprootDeniabilityProof {
    /// P2TR contract outpoint.
    pub contract_outpoint: OutPoint,
    /// Transaction containing the P2TR contract output.
    pub contract_tx: Transaction,
    /// Output index of the contract output.
    pub contract_vout: u32,
    /// Untweaked Taproot internal key.
    pub internal_key: XOnlyPublicKey,
    /// Hashlock leaf script.
    pub hashlock_script: ScriptBuf,
    /// Timelock leaf script.
    pub timelock_script: ScriptBuf,
    /// Claimant's cooperative-path pubkey.
    pub pub_mine_musig: PublicKey,
    /// Counterparty cooperative-path pubkey.
    pub pub_other_musig: PublicKey,
    /// Claimant hashlock pubkey from the script.
    pub pub_mine_hashlock: XOnlyPublicKey,
    /// Schnorr signature by `pub_mine_musig` over the proof message.
    pub sig_musig: secp256k1::schnorr::Signature,
    /// Schnorr signature by `pub_mine_hashlock` over the proof message.
    pub sig_hashlock: secp256k1::schnorr::Signature,
}

/// Legacy deniability proof.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LegacyDeniabilityProof {
    /// Funding outpoint spent by `contract_tx`.
    pub funding_outpoint: OutPoint,
    /// Transaction containing the P2WSH multisig funding output.
    pub funding_tx: Transaction,
    /// Pre-signed contract transaction paying to the HTLC.
    pub contract_tx: Transaction,
    /// HTLC redeemscript.
    pub htlc_redeemscript: ScriptBuf,
    /// 2-of-2 funding multisig redeemscript.
    pub multisig_redeemscript: ScriptBuf,
    /// Claimant multisig pubkey.
    pub pub_mine_multi: PublicKey,
    /// Counterparty multisig pubkey.
    pub pub_other_multi: PublicKey,
    /// Claimant hashlock pubkey from the HTLC.
    pub pub_mine_hashlock: PublicKey,
    /// ECDSA signature by `pub_mine_multi` over the proof message.
    pub sig_multi: ecdsa::Signature,
    /// ECDSA signature by `pub_mine_hashlock` over the proof message.
    pub sig_hashlock: ecdsa::Signature,
}

/// Verification failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeniabilityProofError {
    /// Proof data is malformed or internally inconsistent.
    InvalidProof(&'static str),
    /// On-chain output does not match the proof.
    ChainMismatch(&'static str),
    /// Cryptographic signature verification failed.
    BadSignature(&'static str),
    /// Lower-level protocol or script error.
    General(String),
}

impl fmt::Display for DeniabilityProofError {
    /// Formats proof verification errors for logs and CLI output.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidProof(msg) => write!(f, "invalid proof: {msg}"),
            Self::ChainMismatch(msg) => write!(f, "chain mismatch: {msg}"),
            Self::BadSignature(msg) => write!(f, "bad signature: {msg}"),
            Self::General(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for DeniabilityProofError {}

/// Returns the current Unix timestamp for proof metadata.
fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Builds a stable proof identifier from the protocol, proven outpoint, and direction.
fn proof_id(protocol: ProtocolVersion, outpoint: OutPoint, direction: ProofDirection) -> String {
    format!("{protocol:?}:{outpoint}:{direction:?}")
}

/// Builds the message signed by this wallet to prove control of this swap contract.
fn proof_message(
    protocol: ProtocolVersion,
    outpoint: OutPoint,
    contract_tx: &Transaction,
) -> secp256k1::Message {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"coinswap-deniability-proof-v1");
    bytes.extend_from_slice(format!("{protocol:?}").as_bytes());
    bytes.extend_from_slice(outpoint.to_string().as_bytes());
    bytes.extend_from_slice(contract_tx.compute_txid().to_string().as_bytes());
    bytes.extend_from_slice(&serialize(contract_tx));
    secp256k1::Message::from_digest_slice(sha256::Hash::hash(&bytes).as_byte_array())
        .expect("sha256 digest is always 32 bytes")
}

/// Signs the proof message with a Taproot/Schnorr key.
fn schnorr_sign(privkey: &SecretKey, msg: &secp256k1::Message) -> secp256k1::schnorr::Signature {
    let secp = Secp256k1::new();
    let keypair = Keypair::from_secret_key(&secp, privkey);
    secp.sign_schnorr(msg, &keypair)
}

/// Signs the proof message with a Legacy/ECDSA key using SIGHASH_ALL metadata.
fn ecdsa_sign(privkey: &SecretKey, msg: &secp256k1::Message) -> ecdsa::Signature {
    let secp = Secp256k1::new();
    ecdsa::Signature {
        signature: secp.sign_ecdsa_low_r(msg, privkey),
        sighash_type: bitcoin::sighash::EcdsaSighashType::All,
    }
}

/// Derives a compressed Bitcoin public key from a secret key.
fn secret_key_pubkey(privkey: &SecretKey) -> PublicKey {
    let secp = Secp256k1::new();
    PublicKey {
        compressed: true,
        inner: secp256k1::PublicKey::from_secret_key(&secp, privkey),
    }
}

/// Derives an x-only Taproot public key from a secret key.
fn secret_key_xonly(privkey: &SecretKey) -> XOnlyPublicKey {
    let secp = Secp256k1::new();
    let keypair = Keypair::from_secret_key(&secp, privkey);
    XOnlyPublicKey::from_keypair(&keypair).0
}

impl DeniabilityProof {
    /// Builds a Legacy proof from an incoming swapcoin.
    ///
    /// The caller must pass the funding transaction because legacy swapcoins do
    /// not store it.
    pub(crate) fn from_legacy_incoming_swapcoin(
        swapcoin: &IncomingSwapCoin,
        role: SwapRole,
        funding_tx: Transaction,
        multisig_redeemscript: ScriptBuf,
        outgoing_swapcoin: Option<OutPoint>,
    ) -> Result<Self, WalletError> {
        if swapcoin.protocol != ProtocolVersion::Legacy {
            return Err(WalletError::General(
                "Expected legacy incoming swapcoin".to_string(),
            ));
        }
        let funding_outpoint = swapcoin
            .contract_tx
            .input
            .first()
            .map(|i| i.previous_output)
            .ok_or_else(|| WalletError::General("Contract tx has no inputs".to_string()))?;
        let my_privkey = swapcoin.privkey()?;
        let hashlock_privkey = swapcoin.hashlock_privkey;
        let htlc_redeemscript = swapcoin
            .contract_redeemscript
            .clone()
            .ok_or_else(|| WalletError::General("Missing legacy HTLC redeemscript".to_string()))?;
        let msg = proof_message(
            ProtocolVersion::Legacy,
            funding_outpoint,
            &swapcoin.contract_tx,
        );
        let data = LegacyDeniabilityProof {
            funding_outpoint,
            funding_tx,
            contract_tx: swapcoin.contract_tx.clone(),
            htlc_redeemscript,
            multisig_redeemscript,
            pub_mine_multi: swapcoin.pubkey()?,
            pub_other_multi: swapcoin.other_pubkey()?,
            pub_mine_hashlock: secret_key_pubkey(&hashlock_privkey),
            sig_multi: ecdsa_sign(&my_privkey, &msg),
            sig_hashlock: ecdsa_sign(&hashlock_privkey, &msg),
        };
        Ok(Self::new(
            swapcoin.swap_id.clone().unwrap_or_default(),
            role,
            ProofDirection::Incoming,
            ProtocolVersion::Legacy,
            outgoing_swapcoin,
            DeniabilityProofData::Legacy(data),
            funding_outpoint,
        ))
    }

    /// Builds a Taproot proof from an incoming swapcoin.
    pub(crate) fn from_incoming_swapcoin(
        swapcoin: &IncomingSwapCoin,
        role: SwapRole,
        outgoing_swapcoin: Option<OutPoint>,
    ) -> Result<Self, WalletError> {
        if swapcoin.protocol != ProtocolVersion::Taproot {
            return Err(WalletError::General(
                "Legacy incoming deniability proof requires funding tx context".to_string(),
            ));
        }

        let contract_vout = swapcoin.get_contract_output_vout();
        let contract_outpoint = OutPoint {
            txid: swapcoin.contract_tx.compute_txid(),
            vout: contract_vout,
        };
        let my_privkey = swapcoin.privkey()?;
        let hashlock_privkey = swapcoin.hashlock_privkey;
        let hashlock_script = swapcoin
            .hashlock_script
            .clone()
            .ok_or_else(|| WalletError::General("Missing Taproot hashlock script".to_string()))?;
        let timelock_script = swapcoin
            .timelock_script
            .clone()
            .ok_or_else(|| WalletError::General("Missing Taproot timelock script".to_string()))?;
        let internal_key = swapcoin.internal_key()?;
        let msg = proof_message(
            ProtocolVersion::Taproot,
            contract_outpoint,
            &swapcoin.contract_tx,
        );
        let pub_mine_hashlock = secret_key_xonly(&hashlock_privkey);
        let data = TaprootDeniabilityProof {
            contract_outpoint,
            contract_tx: swapcoin.contract_tx.clone(),
            contract_vout,
            internal_key,
            hashlock_script,
            timelock_script,
            pub_mine_musig: swapcoin.pubkey()?,
            pub_other_musig: swapcoin.other_pubkey()?,
            pub_mine_hashlock,
            sig_musig: schnorr_sign(&my_privkey, &msg),
            sig_hashlock: schnorr_sign(&hashlock_privkey, &msg),
        };
        Ok(Self::new(
            swapcoin.swap_id.clone().unwrap_or_default(),
            role,
            ProofDirection::Incoming,
            ProtocolVersion::Taproot,
            outgoing_swapcoin,
            DeniabilityProofData::Taproot(data),
            contract_outpoint,
        ))
    }

    /// Wraps the protocol-specific proof data.
    fn new(
        swap_id: String,
        role: SwapRole,
        direction: ProofDirection,
        protocol: ProtocolVersion,
        outgoing_swapcoin: Option<OutPoint>,
        proof: DeniabilityProofData,
        key_outpoint: OutPoint,
    ) -> Self {
        Self {
            proof_id: proof_id(protocol, key_outpoint, direction),
            swap_id,
            role,
            direction,
            protocol,
            outgoing_swapcoin,
            proof,
            created_at: now_unix_secs(),
        }
    }
}

/// Verify a proof. Pass `chain_output` to also check the real chain output.
pub fn verify_proof(
    proof: &DeniabilityProof,
    chain_output: Option<&TxOut>,
) -> Result<(), DeniabilityProofError> {
    match &proof.proof {
        DeniabilityProofData::Taproot(data) => verify_taproot_proof(data, chain_output),
        DeniabilityProofData::Legacy(data) => verify_legacy_proof(data, chain_output),
    }
}

/// Verifies a Taproot proof.
fn verify_taproot_proof(
    proof: &TaprootDeniabilityProof,
    chain_output: Option<&TxOut>,
) -> Result<(), DeniabilityProofError> {
    let contract_output = proof
        .contract_tx
        .output
        .get(proof.contract_vout as usize)
        .ok_or(DeniabilityProofError::InvalidProof(
            "contract_vout is outside contract_tx outputs",
        ))?;
    if proof.contract_outpoint.txid != proof.contract_tx.compute_txid()
        || proof.contract_outpoint.vout != proof.contract_vout
    {
        return Err(DeniabilityProofError::InvalidProof(
            "contract outpoint does not match contract tx",
        ));
    }

    // Rebuild the P2TR script and compare it with the contract output.
    let (script_pubkey, _) = create_taproot_script(
        proof.hashlock_script.clone(),
        proof.timelock_script.clone(),
        proof.internal_key,
    )
    .map_err(|e| DeniabilityProofError::General(format!("{e:?}")))?;
    if contract_output.script_pubkey != script_pubkey {
        return Err(DeniabilityProofError::InvalidProof(
            "contract tx output does not match Taproot scripts",
        ));
    }
    if let Some(chain_output) = chain_output {
        if chain_output.script_pubkey != script_pubkey
            || chain_output.value != contract_output.value
        {
            return Err(DeniabilityProofError::ChainMismatch(
                "chain output does not match proof output",
            ));
        }
    }

    // The internal key must be the aggregate of the two MuSig keys.
    let mut ordered = [proof.pub_mine_musig, proof.pub_other_musig];
    ordered.sort_by_key(|k| k.inner.serialize());
    let aggregate = get_aggregated_pubkey_compat(ordered[0].inner, ordered[1].inner)
        .map_err(|e| DeniabilityProofError::General(format!("{e:?}")))?;
    if aggregate != proof.internal_key {
        return Err(DeniabilityProofError::InvalidProof(
            "MuSig aggregate does not match internal key",
        ));
    }

    // The hashlock leaf must contain this wallet's hashlock key.
    let script_hashlock_key = taproot_hashlock_pubkey_for_verify(&proof.hashlock_script)?;
    if script_hashlock_key != proof.pub_mine_hashlock {
        return Err(DeniabilityProofError::InvalidProof(
            "hashlock script does not contain claimant key",
        ));
    }

    let msg = proof_message(
        ProtocolVersion::Taproot,
        proof.contract_outpoint,
        &proof.contract_tx,
    );
    let secp = Secp256k1::new();
    let (musig_xonly, _) = proof.pub_mine_musig.inner.x_only_public_key();
    secp.verify_schnorr(&proof.sig_musig, &msg, &musig_xonly)
        .map_err(|_| DeniabilityProofError::BadSignature("musig signature"))?;
    secp.verify_schnorr(&proof.sig_hashlock, &msg, &proof.pub_mine_hashlock)
        .map_err(|_| DeniabilityProofError::BadSignature("hashlock signature"))?;
    Ok(())
}

/// Extracts the claimant hashlock key from the Taproot hashlock leaf script.
fn taproot_hashlock_pubkey_for_verify(
    script: &ScriptBuf,
) -> Result<XOnlyPublicKey, DeniabilityProofError> {
    let instructions: Vec<_> = script.instructions().collect();
    match instructions.get(3) {
        Some(Ok(bitcoin::script::Instruction::PushBytes(bytes))) => {
            XOnlyPublicKey::from_slice(bytes.as_bytes()).map_err(|e| {
                DeniabilityProofError::General(format!("invalid hashlock pubkey: {e:?}"))
            })
        }
        _ => Err(DeniabilityProofError::InvalidProof(
            "Taproot hashlock script missing pubkey",
        )),
    }
}

/// Verifies a Legacy proof.
fn verify_legacy_proof(
    proof: &LegacyDeniabilityProof,
    chain_output: Option<&TxOut>,
) -> Result<(), DeniabilityProofError> {
    let input = proof
        .contract_tx
        .input
        .first()
        .ok_or(DeniabilityProofError::InvalidProof(
            "contract tx has no inputs",
        ))?;
    if input.previous_output != proof.funding_outpoint {
        return Err(DeniabilityProofError::InvalidProof(
            "contract tx does not spend funding outpoint",
        ));
    }
    if proof.funding_tx.compute_txid() != proof.funding_outpoint.txid {
        return Err(DeniabilityProofError::InvalidProof(
            "funding txid does not match funding outpoint",
        ));
    }
    // Check that the funding output pays to the 2-of-2 redeemscript.
    let funding_output = proof
        .funding_tx
        .output
        .get(proof.funding_outpoint.vout as usize)
        .ok_or(DeniabilityProofError::InvalidProof(
            "funding vout is outside funding tx outputs",
        ))?;
    let multisig_spk = redeemscript_to_scriptpubkey(&proof.multisig_redeemscript)
        .map_err(|e| DeniabilityProofError::General(format!("{e:?}")))?;
    if funding_output.script_pubkey != multisig_spk {
        return Err(DeniabilityProofError::InvalidProof(
            "funding output does not pay to multisig redeemscript",
        ));
    }
    if let Some(chain_output) = chain_output {
        if chain_output.script_pubkey != multisig_spk || chain_output.value != funding_output.value
        {
            return Err(DeniabilityProofError::ChainMismatch(
                "chain output does not match proof funding output",
            ));
        }
    }
    // Check that the contract transaction pays to the HTLC redeemscript.
    let contract_output =
        proof
            .contract_tx
            .output
            .first()
            .ok_or(DeniabilityProofError::InvalidProof(
                "contract tx has no outputs",
            ))?;
    let htlc_spk = redeemscript_to_scriptpubkey(&proof.htlc_redeemscript)
        .map_err(|e| DeniabilityProofError::General(format!("{e:?}")))?;
    if contract_output.script_pubkey != htlc_spk {
        return Err(DeniabilityProofError::InvalidProof(
            "contract output does not pay to HTLC redeemscript",
        ));
    }

    // Check the multisig keys.
    let (multi_a, multi_b) = read_pubkeys_from_multisig_redeemscript(&proof.multisig_redeemscript)
        .map_err(|e| DeniabilityProofError::General(format!("{e:?}")))?;
    if !((proof.pub_mine_multi == multi_a && proof.pub_other_multi == multi_b)
        || (proof.pub_mine_multi == multi_b && proof.pub_other_multi == multi_a))
    {
        return Err(DeniabilityProofError::InvalidProof(
            "multisig redeemscript does not contain expected keys",
        ));
    }

    // Check the hashlock key.
    let hashlock_key = read_hashlock_pubkey_from_contract(&proof.htlc_redeemscript)
        .map_err(|e| DeniabilityProofError::General(format!("{e:?}")))?;
    if hashlock_key != proof.pub_mine_hashlock {
        return Err(DeniabilityProofError::InvalidProof(
            "HTLC redeemscript does not contain claimant hashlock key",
        ));
    }

    let msg = proof_message(
        ProtocolVersion::Legacy,
        proof.funding_outpoint,
        &proof.contract_tx,
    );
    let secp = Secp256k1::new();
    let multi_sig =
        SecpEcdsaSignature::from_compact(&proof.sig_multi.signature.serialize_compact())
            .map_err(|e| DeniabilityProofError::General(format!("{e:?}")))?;
    secp.verify_ecdsa(&msg, &multi_sig, &proof.pub_mine_multi.inner)
        .map_err(|_| DeniabilityProofError::BadSignature("multisig signature"))?;
    let hashlock_sig =
        SecpEcdsaSignature::from_compact(&proof.sig_hashlock.signature.serialize_compact())
            .map_err(|e| DeniabilityProofError::General(format!("{e:?}")))?;
    secp.verify_ecdsa(&msg, &hashlock_sig, &proof.pub_mine_hashlock.inner)
        .map_err(|_| DeniabilityProofError::BadSignature("hashlock signature"))?;
    Ok(())
}

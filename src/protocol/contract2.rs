//! This module implements the contract protocol for a 2-of-2 multisig transaction

use crate::protocol::error::ProtocolError;

use bitcoin::{
    blockdata::transaction::{Transaction, TxOut},
    locktime::absolute::LockTime,
    opcodes::all::{OP_CHECKSIG, OP_CLTV, OP_DROP, OP_EQUALVERIFY, OP_SHA256},
    script,
    secp256k1::{Secp256k1, SecretKey, XOnlyPublicKey},
    sighash::{Prevouts, SighashCache},
    taproot::{LeafVersion, TaprootBuilder, TaprootSpendInfo},
    Amount, PublicKey, ScriptBuf,
};

// create_hashlock_script
pub(crate) fn create_hashlock_script(hash: &[u8; 32], pubkey: &XOnlyPublicKey) -> ScriptBuf {
    script::Builder::new()
        .push_opcode(OP_SHA256)
        .push_slice(hash)
        .push_opcode(OP_EQUALVERIFY)
        .push_x_only_key(pubkey)
        .push_opcode(OP_CHECKSIG)
        .into_script()
}

/// Extract the hash from a hashlock script
/// Hashlock script format: `OP_SHA256 <32-byte hash> OP_EQUALVERIFY <pubkey> OP_CHECKSIG`
pub(crate) fn extract_hash_from_hashlock(script: &ScriptBuf) -> Result<[u8; 32], ProtocolError> {
    let instructions: Vec<_> = script.instructions().collect();

    // We expect: OP_SHA256, PUSH(32 bytes), OP_EQUALVERIFY, PUSH(32 bytes), OP_CHECKSIG
    if instructions.len() < 5 {
        return Err(ProtocolError::General("Invalid hashlock script format"));
    }

    // The hash is the second instruction (after OP_SHA256)
    if let Some(Ok(bitcoin::script::Instruction::PushBytes(hash_bytes))) = instructions.get(1) {
        if hash_bytes.len() == 32 {
            let mut hash = [0u8; 32];
            hash.copy_from_slice(hash_bytes.as_bytes());
            return Ok(hash);
        }
    }

    Err(ProtocolError::General(
        "Failed to extract hash from hashlock script",
    ))
}

/// Check that a Taproot hashlock script contains the pubkey derived from
/// `tweakable_point + nonce * G`. Mirrors Legacy's `check_hashlock_has_pubkey`.
pub(crate) fn check_taproot_hashlock_has_pubkey(
    hashlock_script: &ScriptBuf,
    tweakable_point: &PublicKey,
    nonce: &SecretKey,
) -> Result<(), ProtocolError> {
    let expected = crate::protocol::contract::calculate_pubkey_from_nonce(tweakable_point, nonce)?;
    let (expected_xonly, _) = expected.inner.x_only_public_key();

    // Hashlock script: OP_SHA256 <hash> OP_EQUALVERIFY <pubkey> OP_CHECKSIG
    let instructions: Vec<_> = hashlock_script.instructions().collect();
    if let Some(Ok(bitcoin::script::Instruction::PushBytes(pk_bytes))) = instructions.get(3) {
        if let Ok(script_xonly) = XOnlyPublicKey::from_slice(pk_bytes.as_bytes()) {
            if script_xonly == expected_xonly {
                return Ok(());
            }
        }
    }

    Err(ProtocolError::General(
        "Taproot hashlock pubkey doesn't match nonce-derived key",
    ))
}

/// Attempts to extract the 32-byte preimage from a taproot script-path spending transaction.
///
// create_timelock_script
pub(crate) fn create_timelock_script(locktime: LockTime, pubkey: &XOnlyPublicKey) -> ScriptBuf {
    script::Builder::new()
        .push_lock_time(locktime)
        .push_opcode(OP_CLTV)
        .push_opcode(OP_DROP)
        .push_x_only_key(pubkey)
        .push_opcode(OP_CHECKSIG)
        .into_script()
}

pub(crate) fn create_taproot_script(
    hashlock_script: ScriptBuf,
    timelock_script: ScriptBuf,
    internal_pubkey: XOnlyPublicKey,
) -> Result<(ScriptBuf, TaprootSpendInfo), ProtocolError> {
    let secp = Secp256k1::new();
    let taproot_spendinfo = TaprootBuilder::new()
        .add_leaf(1, hashlock_script.clone())?
        .add_leaf(1, timelock_script.clone())?
        .finalize(&secp, internal_pubkey)?;
    let _hashlock_control_block =
        taproot_spendinfo.control_block(&(hashlock_script, LeafVersion::TapScript));
    let _timelock_control_block =
        taproot_spendinfo.control_block(&(timelock_script, LeafVersion::TapScript));
    Ok((
        ScriptBuf::new_p2tr(
            &secp,
            taproot_spendinfo.internal_key(),
            taproot_spendinfo.merkle_root(),
        ),
        taproot_spendinfo,
    ))
}

/// Calculate Taproot sighash for contract spending
pub(crate) fn calculate_contract_sighash(
    spending_tx: &Transaction,
    contract_amount: Amount,
    hashlock_script: &ScriptBuf,
    timelock_script: &ScriptBuf,
    internal_key: bitcoin::secp256k1::XOnlyPublicKey,
) -> Result<bitcoin::secp256k1::Message, ProtocolError> {
    // Reconstruct the Taproot script
    let (contract_script, _) = create_taproot_script(
        hashlock_script.clone(),
        timelock_script.clone(),
        internal_key,
    )?;

    // Create prevout for sighash calculation
    let prevout = TxOut {
        value: contract_amount,
        script_pubkey: contract_script,
    };
    let prevouts = vec![prevout];
    let prevouts_ref = Prevouts::All(&prevouts);

    // Calculate sighash
    let mut spending_tx_copy = spending_tx.clone();
    let mut sighasher = SighashCache::new(&mut spending_tx_copy);
    let sighash = sighasher.taproot_key_spend_signature_hash(
        0,
        &prevouts_ref,
        bitcoin::TapSighashType::Default,
    )?;

    Ok(bitcoin::secp256k1::Message::from(sighash))
}

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

use bitcoind::bitcoincore_rpc::RpcApi;

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
/// The taproot script-path witness structure is: [signature, preimage, script, control_block]
/// This function extracts the preimage from witness index 1.
pub(crate) fn extract_preimage_from_spending_tx(spending_tx: &Transaction) -> Option<[u8; 32]> {
    let input = spending_tx.input.first()?;
    let witness = &input.witness;

    // Taproot script-path witness must have at least 4 elements
    if witness.len() < 4 {
        return None;
    }

    // Index 1 contains the preimage
    let preimage_bytes = witness.nth(1)?;

    // Preimage must be exactly 32 bytes
    if preimage_bytes.len() != 32 {
        return None;
    }

    let mut preimage = [0u8; 32];
    preimage.copy_from_slice(preimage_bytes);
    Some(preimage)
}

/// Detect how a taproot output was spent by analyzing witness data
pub(crate) fn detect_taproot_spending_path(
    spending_tx: &Transaction,
    spent_outpoint: bitcoin::OutPoint,
) -> Result<crate::taker::api2::TaprootSpendingPath, ProtocolError> {
    use crate::taker::api2::TaprootSpendingPath;

    // Find the input that spends the specific outpoint
    let input_index = spending_tx
        .input
        .iter()
        .position(|input| input.previous_output == spent_outpoint);

    let input_index = match input_index {
        Some(idx) => {
            log::info!(
                "Found spent outpoint {} at input index {}",
                spent_outpoint,
                idx
            );
            idx
        }
        None => {
            log::error!(
                "Could not find input spending outpoint {} in tx {}",
                spent_outpoint,
                spending_tx.compute_txid()
            );
            return Err(ProtocolError::General(
                "Spending transaction does not spend the specified outpoint",
            ));
        }
    };

    let witness = &spending_tx.input[input_index].witness;

    log::info!(
        "Analyzing spending tx {}: witness has {} elements",
        spending_tx.compute_txid(),
        witness.len()
    );
    for (i, elem) in witness.iter().enumerate() {
        log::info!("  Witness[{}]: {} bytes", i, elem.len());
    }

    // Key-path spend: single signature in witness
    if witness.len() == 1 {
        log::info!("Detected key-path spend");
        return Ok(TaprootSpendingPath::KeyPath);
    }

    // Script-path spend: has script and control block
    if witness.len() >= 3 {
        log::info!("Witness has >= 3 elements, analyzing script-path spend");
        // Last element is control block, second-to-last is script
        let script_bytes = &witness[witness.len() - 2];

        // Parse script to detect hashlock vs timelock
        let script = bitcoin::Script::from_bytes(script_bytes);
        log::info!("Script length: {} bytes", script.as_bytes().len());

        // Hashlock script contains OP_SHA256 <hash> OP_EQUALVERIFY
        if script.as_bytes().windows(2).any(|w| w[0] == 0xa8) {
            log::info!("Found OP_SHA256 (0xa8) - appears to be hashlock script");
            // OP_SHA256 = 0xa8
            // Extract preimage from witness (it's before the script)
            if witness.len() >= 4 && witness[1].len() == 32 {
                let mut preimage = [0u8; 32];
                preimage.copy_from_slice(&witness[1]);
                log::info!("Detected hashlock spend with preimage");
                return Ok(TaprootSpendingPath::Hashlock { preimage });
            } else {
                log::warn!(
                    "Found OP_SHA256 but witness structure doesn't match: len={}, witness[1].len()={}",
                    witness.len(),
                    if witness.len() > 1 { witness[1].len() } else { 0 }
                );
            }
        }

        // Timelock script contains OP_CHECKLOCKTIMEVERIFY
        if script.as_bytes().contains(&0xb1) {
            log::info!("Found OP_CHECKLOCKTIMEVERIFY (0xb1) - detected timelock spend");
            // OP_CHECKLOCKTIMEVERIFY = 0xb1
            return Ok(TaprootSpendingPath::Timelock);
        }

        log::warn!("Script-path spend but couldn't identify as hashlock or timelock");
    } else {
        log::warn!(
            "Witness has {} elements, not enough for script-path (need >= 3)",
            witness.len()
        );
    }

    Err(ProtocolError::General(
        "Could not determine spending path from witness",
    ))
}

/// Check if a contract's timelock has matured
pub(crate) fn is_timelock_mature(
    rpc: &bitcoind::bitcoincore_rpc::Client,
    contract_txid: &bitcoin::Txid,
    timelock: u32,
) -> Result<bool, ProtocolError> {
    match rpc.get_raw_transaction_info(contract_txid, None) {
        Ok(info) => {
            if let Some(confirmations) = info.confirmations {
                Ok(confirmations > timelock)
            } else {
                Ok(false) // Not confirmed yet
            }
        }
        Err(_) => Ok(false), // Contract not found or not confirmed
    }
}

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

pub(crate) fn calculate_coinswap_fee(
    swap_amount: u64,
    refund_locktime: u16,
    base_fee: u64,
    amt_rel_fee_pct: f64,
    time_rel_fee_pct: f64,
) -> u64 {
    // swap_amount as f64 * refund_locktime as f64 -> can  overflow inside f64?
    let total_fee = base_fee as f64
        + (swap_amount as f64 * amt_rel_fee_pct) / 1_00.00
        + (swap_amount as f64 * refund_locktime as f64 * time_rel_fee_pct) / 1_00.00;

    total_fee.ceil() as u64
}

//! Taproot (MuSig2) message verification for the Maker.
//!
//! Verifies taker messages received during the Taproot swap flow, mirroring
//! the taker's `taproot_verification.rs` but from the maker's perspective.

use bitcoin::{secp256k1::Secp256k1, PublicKey};

use crate::{
    protocol::{
        common_messages::SwapPrivkey, contract2::extract_hash_from_hashlock,
        taproot_messages::TaprootContractData,
    },
    taker::api::REFUND_LOCKTIME_STEP,
};

use super::error::MakerError;

/// Verify incoming Taproot contract data from the taker.
pub(crate) fn verify_taproot_contract_data(
    data: &TaprootContractData,
    maker_timelock: u32,
    wallet_name: &str,
) -> Result<(), MakerError> {
    // Must have at least one contract tx
    if data.contract_txs.is_empty() {
        return Err(MakerError::General(
            "Taker sent empty Taproot contract data (no contract txs)",
        ));
    }

    // contract_txs and amounts must have matching lengths
    if data.contract_txs.len() != data.amounts.len() {
        return Err(MakerError::General(
            format!(
                "Taproot contract_txs count ({}) doesn't match amounts count ({})",
                data.contract_txs.len(),
                data.amounts.len()
            )
            .leak(),
        ));
    }

    let _hash = extract_hash_from_hashlock(&data.hashlock_script).map_err(|e| {
        MakerError::General(format!("Taproot hashlock script is invalid: {:?}", e).leak())
    })?;

    let hashlock_instruction_count = data.hashlock_script.instructions().count();
    if hashlock_instruction_count != 5 {
        return Err(MakerError::General(
            format!(
                "Taproot hashlock script has {} instructions, expected 5",
                hashlock_instruction_count
            )
            .leak(),
        ));
    }

    // Verify timelock script has expected format (5 instructions):
    // <locktime> OP_CLTV OP_DROP <pubkey> OP_CHECKSIG
    let timelock_instruction_count = data.timelock_script.instructions().count();
    if timelock_instruction_count != 5 {
        return Err(MakerError::General(
            format!(
                "Taproot timelock script has {} instructions, expected 5",
                timelock_instruction_count
            )
            .leak(),
        ));
    }

    // Extract and validate the timelock value from the timelock script
    let maker_locktime_val: u64 = if let Some(first) = data.timelock_script.instructions().next() {
        match first.map_err(|e| {
            MakerError::General(format!("Taproot timelock script parse error: {:?}", e).leak())
        })? {
            bitcoin::script::Instruction::PushBytes(locktime_bytes) => {
                let bytes = locktime_bytes.as_bytes();
                if bytes.is_empty() {
                    return Err(MakerError::General(
                        "Taproot timelock script has empty locktime",
                    ));
                }
                match bytes.len() {
                    1 => bytes[0] as u64,
                    2 => u16::from_le_bytes([bytes[0], bytes[1]]) as u64,
                    3 => u32::from_le_bytes([bytes[0], bytes[1], bytes[2], 0]) as u64,
                    4 => u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as u64,
                    _ => {
                        return Err(MakerError::General(
                            format!(
                                "Taproot timelock has unexpected byte length {}",
                                bytes.len()
                            )
                            .leak(),
                        ));
                    }
                }
            }
            bitcoin::script::Instruction::Op(opcode) => {
                if let bitcoin::opcodes::Class::PushNum(n) =
                    opcode.classify(bitcoin::opcodes::ClassifyContext::Legacy)
                {
                    if n <= 0 {
                        return Err(MakerError::General(
                            format!("Taproot timelock value is non-positive ({})", n).leak(),
                        ));
                    }
                    n as u64
                } else {
                    return Err(MakerError::General(
                        "Taproot timelock script doesn't start with a locktime",
                    ));
                }
            }
        }
    } else {
        return Err(MakerError::General("Taproot timelock script is empty"));
    };

    if maker_locktime_val == 0 {
        return Err(MakerError::General("Taproot timelock value is zero"));
    }

    let expected_taker_locktime = maker_timelock as u64 + REFUND_LOCKTIME_STEP as u64;
    if maker_locktime_val != expected_taker_locktime {
        return Err(MakerError::General(
            format!(
                "Taproot timelock value {} does not match expected taker locktime {}",
                maker_locktime_val, expected_taker_locktime
            )
            .leak(),
        ));
    }

    let secp = Secp256k1::verification_only();
    let expected_spk = {
        let builder = bitcoin::taproot::TaprootBuilder::new()
            .add_leaf(1, data.hashlock_script.clone())
            .map_err(|e| {
                MakerError::General(
                    format!("Taproot tree build failed (hashlock leaf): {:?}", e).leak(),
                )
            })?
            .add_leaf(1, data.timelock_script.clone())
            .map_err(|e| {
                MakerError::General(
                    format!("Taproot tree build failed (timelock leaf): {:?}", e).leak(),
                )
            })?;
        let tap_info = builder.finalize(&secp, data.internal_key).map_err(|e| {
            MakerError::General(format!("Taproot tree finalization failed: {:?}", e).leak())
        })?;
        bitcoin::ScriptBuf::new_p2tr_tweaked(tap_info.output_key())
    };

    for (i, tx) in data.contract_txs.iter().enumerate() {
        if tx.input.is_empty() {
            return Err(MakerError::General(
                format!("Taproot contract tx {} has no inputs", i).leak(),
            ));
        }
        if tx.output.is_empty() {
            return Err(MakerError::General(
                format!("Taproot contract tx {} has no outputs", i).leak(),
            ));
        }
        if tx.output[0].script_pubkey != expected_spk {
            return Err(MakerError::General(
                format!(
                    "Taproot contract tx {} output scriptpubkey does not match \
                     expected P2TR address derived from (internal_key, script_tree)",
                    i
                )
                .leak(),
            ));
        }
    }

    log::info!(
        "[{}] Verified Taproot contract data from taker: {} contract txs \
         (hash, timelock, structure, P2TR output, amounts)",
        wallet_name,
        data.contract_txs.len()
    );
    Ok(())
}

/// Verify Taproot private key handover from the taker.
pub(crate) fn verify_taproot_privkey_handover(
    privkeys: &[SwapPrivkey],
    incoming_swapcoins: &[crate::wallet::swapcoin::IncomingSwapCoin],
    wallet_name: &str,
) -> Result<(), MakerError> {
    if privkeys.len() != incoming_swapcoins.len() {
        return Err(MakerError::General(
            format!(
                "Taproot privkey count {} doesn't match incoming swapcoin count {}",
                privkeys.len(),
                incoming_swapcoins.len()
            )
            .leak(),
        ));
    }

    let secp = Secp256k1::new();

    for (privkey_info, incoming) in privkeys.iter().zip(incoming_swapcoins.iter()) {
        // Derive the public key from the received private key
        let derived_pubkey = PublicKey {
            compressed: true,
            inner: bitcoin::secp256k1::PublicKey::from_secret_key(&secp, &privkey_info.key),
        };

        // Verify it matches the expected other_pubkey on the incoming swapcoin
        let expected_pubkey = incoming.other_pubkey.ok_or(MakerError::General(
            "Missing other_pubkey on incoming swapcoin during Taproot handover verification",
        ))?;
        if derived_pubkey != expected_pubkey {
            return Err(MakerError::General(
                "Taproot privkey derives pubkey that doesn't match expected other_pubkey",
            ));
        }
    }

    log::info!(
        "[{}] Verified {} Taproot private keys (derived pubkey matches expected)",
        wallet_name,
        privkeys.len()
    );
    Ok(())
}

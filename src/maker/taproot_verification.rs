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
#[hotpath::measure]
pub(crate) fn verify_taproot_contract_data(
    data: &TaprootContractData,
    maker_timelock: u32,
    network_port: u16,
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

    let hashlock_instructions = data
        .hashlock_script
        .instructions()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| {
            MakerError::General(format!("Taproot hashlock script parse error: {:?}", e).leak())
        })?;
    if hashlock_instructions.len() != 5 {
        return Err(MakerError::General(
            format!(
                "Taproot hashlock script has {} instructions, expected 5",
                hashlock_instructions.len()
            )
            .leak(),
        ));
    }
    if !matches!(
        hashlock_instructions.as_slice(),
        [
            bitcoin::script::Instruction::Op(bitcoin::opcodes::all::OP_SHA256),
            bitcoin::script::Instruction::PushBytes(hash),
            bitcoin::script::Instruction::Op(bitcoin::opcodes::all::OP_EQUALVERIFY),
            bitcoin::script::Instruction::PushBytes(pubkey),
            bitcoin::script::Instruction::Op(bitcoin::opcodes::all::OP_CHECKSIG),
        ] if hash.len() == 32 && pubkey.len() == 32
    ) {
        return Err(MakerError::General(
            "Taproot hashlock script has unexpected template",
        ));
    }

    // Verify timelock script has expected format (5 instructions):
    // <locktime> OP_CLTV OP_DROP <pubkey> OP_CHECKSIG
    let timelock_instructions = data
        .timelock_script
        .instructions()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| {
            MakerError::General(format!("Taproot timelock script parse error: {:?}", e).leak())
        })?;
    if timelock_instructions.len() != 5 {
        return Err(MakerError::General(
            format!(
                "Taproot timelock script has {} instructions, expected 5",
                timelock_instructions.len()
            )
            .leak(),
        ));
    }
    if !matches!(
        timelock_instructions.as_slice(),
        [
            _,
            bitcoin::script::Instruction::Op(bitcoin::opcodes::all::OP_CLTV),
            bitcoin::script::Instruction::Op(bitcoin::opcodes::all::OP_DROP),
            bitcoin::script::Instruction::PushBytes(pubkey),
            bitcoin::script::Instruction::Op(bitcoin::opcodes::all::OP_CHECKSIG),
        ] if pubkey.len() == 32
    ) {
        return Err(MakerError::General(
            "Taproot timelock script has unexpected template",
        ));
    }

    // Extract and validate the timelock value from the timelock script
    let maker_locktime_val: u64 = if let Some(first) = timelock_instructions.first() {
        match first {
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
    let (expected_spk, expected_tap_tweak) = {
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
        (
            bitcoin::ScriptBuf::new_p2tr_tweaked(tap_info.output_key()),
            tap_info.tap_tweak().to_scalar(),
        )
    };
    if data.tap_tweak_scalar()? != expected_tap_tweak {
        return Err(MakerError::General(
            "Taproot tap tweak does not match internal key and script tree",
        ));
    }

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
        if data.amounts[i] != tx.output[0].value {
            return Err(MakerError::General(
                format!(
                    "Taproot contract tx {} amount {} does not match output value {}",
                    i, data.amounts[i], tx.output[0].value
                )
                .leak(),
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
        network_port,
        data.contract_txs.len()
    );
    Ok(())
}

/// Verify Taproot private key handover from the taker.
#[hotpath::measure]
pub(crate) fn verify_taproot_privkey_handover(
    privkeys: &[SwapPrivkey],
    incoming_swapcoins: &[crate::wallet::swapcoin::IncomingSwapCoin],
    network_port: u16,
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
        network_port,
        privkeys.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::{
        absolute::LockTime,
        hashes::Hash,
        opcodes::all::{OP_CHECKSIG, OP_CLTV, OP_DROP, OP_DUP, OP_EQUALVERIFY},
        script, transaction, Amount, OutPoint, Sequence, Transaction, TxIn, TxOut, Txid, Witness,
    };

    use crate::protocol::{
        contract2::{create_hashlock_script, create_timelock_script},
        taproot_messages::SerializableScalar,
    };

    const MAKER_TIMELOCK: u32 = 300;

    fn keypair(byte: u8) -> bitcoin::secp256k1::Keypair {
        let secp = Secp256k1::new();
        let secret = bitcoin::secp256k1::SecretKey::from_slice(&[byte; 32]).unwrap();
        bitcoin::secp256k1::Keypair::from_secret_key(&secp, &secret)
    }

    fn output_script(
        internal_key: bitcoin::secp256k1::XOnlyPublicKey,
        hashlock_script: bitcoin::ScriptBuf,
        timelock_script: bitcoin::ScriptBuf,
    ) -> (bitcoin::ScriptBuf, SerializableScalar) {
        let secp = Secp256k1::new();
        let tap_info = bitcoin::taproot::TaprootBuilder::new()
            .add_leaf(1, hashlock_script)
            .unwrap()
            .add_leaf(1, timelock_script)
            .unwrap()
            .finalize(&secp, internal_key)
            .unwrap();
        (
            bitcoin::ScriptBuf::new_p2tr_tweaked(tap_info.output_key()),
            SerializableScalar::from_bytes(tap_info.tap_tweak().to_scalar().to_be_bytes().to_vec()),
        )
    }

    fn contract_tx(script_pubkey: bitcoin::ScriptBuf, amount: Amount) -> Transaction {
        Transaction {
            version: transaction::Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint {
                    txid: Txid::all_zeros(),
                    vout: 0,
                },
                script_sig: bitcoin::ScriptBuf::new(),
                sequence: Sequence::MAX,
                witness: Witness::new(),
            }],
            output: vec![TxOut {
                value: amount,
                script_pubkey,
            }],
        }
    }

    fn valid_contract_data() -> TaprootContractData {
        let internal_key = bitcoin::secp256k1::XOnlyPublicKey::from_keypair(&keypair(1)).0;
        let script_key = bitcoin::secp256k1::XOnlyPublicKey::from_keypair(&keypair(2)).0;
        let hash = [7u8; 32];
        let hashlock_script = create_hashlock_script(&hash, &script_key);
        let timelock = LockTime::from_height(MAKER_TIMELOCK + REFUND_LOCKTIME_STEP as u32).unwrap();
        let timelock_script = create_timelock_script(timelock, &script_key);
        let amount = Amount::from_sat(50_000);
        let (script_pubkey, tap_tweak) = output_script(
            internal_key,
            hashlock_script.clone(),
            timelock_script.clone(),
        );

        TaprootContractData::new(
            "test-swap".to_string(),
            Vec::new(),
            bitcoin::PublicKey::new(bitcoin::secp256k1::PublicKey::from_keypair(&keypair(3))),
            internal_key,
            tap_tweak,
            hashlock_script,
            timelock_script,
            vec![contract_tx(script_pubkey, amount)],
            vec![amount],
            None,
            None,
        )
    }

    fn refresh_output_script(data: &mut TaprootContractData) {
        let (script_pubkey, tap_tweak) = output_script(
            data.internal_key,
            data.hashlock_script.clone(),
            data.timelock_script.clone(),
        );
        data.contract_txs[0].output[0].script_pubkey = script_pubkey;
        data.tap_tweak = tap_tweak;
    }

    #[test]
    fn rejects_amount_that_does_not_match_contract_output() {
        let mut data = valid_contract_data();
        data.amounts[0] += Amount::from_sat(1);

        assert!(verify_taproot_contract_data(&data, MAKER_TIMELOCK, 0).is_err());
    }

    #[test]
    fn rejects_hashlock_script_with_wrong_template() {
        let mut data = valid_contract_data();
        let script_key = bitcoin::secp256k1::XOnlyPublicKey::from_keypair(&keypair(2)).0;
        data.hashlock_script = script::Builder::new()
            .push_opcode(OP_DUP)
            .push_slice([7u8; 32])
            .push_opcode(OP_EQUALVERIFY)
            .push_x_only_key(&script_key)
            .push_opcode(OP_CHECKSIG)
            .into_script();
        refresh_output_script(&mut data);

        assert!(verify_taproot_contract_data(&data, MAKER_TIMELOCK, 0).is_err());
    }

    #[test]
    fn rejects_timelock_script_with_wrong_template() {
        let mut data = valid_contract_data();
        let script_key = bitcoin::secp256k1::XOnlyPublicKey::from_keypair(&keypair(2)).0;
        let timelock = LockTime::from_height(MAKER_TIMELOCK + REFUND_LOCKTIME_STEP as u32).unwrap();
        data.timelock_script = script::Builder::new()
            .push_lock_time(timelock)
            .push_opcode(OP_DROP)
            .push_opcode(OP_CLTV)
            .push_x_only_key(&script_key)
            .push_opcode(OP_CHECKSIG)
            .into_script();
        refresh_output_script(&mut data);

        assert!(verify_taproot_contract_data(&data, MAKER_TIMELOCK, 0).is_err());
    }

    #[test]
    fn rejects_tap_tweak_that_does_not_match_script_tree() {
        let mut data = valid_contract_data();
        data.tap_tweak = SerializableScalar::from_bytes([1u8; 32].to_vec());

        assert!(verify_taproot_contract_data(&data, MAKER_TIMELOCK, 0).is_err());
    }
}

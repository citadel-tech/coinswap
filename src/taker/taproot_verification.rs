//! Taproot (MuSig2) message verification for the Unified Taker.
//!
//! Verifies Taproot contract data received from makers during the swap flow.

use bitcoin::{
    hashes::{sha256, Hash},
    secp256k1::Secp256k1,
};

use crate::protocol::{
    contract2::extract_hash_from_hashlock, taproot_messages::TaprootContractData,
};

use super::{error::TakerError, unified_api::UnifiedTaker};

impl UnifiedTaker {
    /// Verify a maker's Taproot contract data response.
    pub(crate) fn verify_maker_taproot_contract(
        &self,
        contract: &TaprootContractData,
        maker_idx: usize,
        expected_locktime: u32,
        min_expected_amount: Option<bitcoin::Amount>,
    ) -> Result<(), TakerError> {
        // Must have at least one contract tx
        if contract.contract_txs.is_empty() {
            return Err(TakerError::General(format!(
                "Maker {} sent empty Taproot contract data (no contract txs)",
                maker_idx
            )));
        }

        // Amounts must be non-zero
        for (i, amount) in contract.amounts.iter().enumerate() {
            if *amount == bitcoin::Amount::ZERO {
                return Err(TakerError::General(format!(
                    "Maker {} Taproot contract amount {} is zero",
                    maker_idx, i
                )));
            }
        }

        // Verify hashlock script contains expected hash
        let expected_hash: [u8; 32] =
            sha256::Hash::hash(&self.swap_state()?.preimage).to_byte_array();
        let actual_hash = extract_hash_from_hashlock(&contract.hashlock_script).map_err(|e| {
            TakerError::General(format!(
                "Maker {} Taproot hashlock script is invalid: {:?}",
                maker_idx, e
            ))
        })?;

        if actual_hash != expected_hash {
            return Err(TakerError::General(format!(
                "Maker {} Taproot hashlock script has wrong hash",
                maker_idx
            )));
        }

        // Verify hashlock script has expected format (5 instructions):
        // OP_SHA256 <hash> OP_EQUALVERIFY <pubkey> OP_CHECKSIG
        let hashlock_instruction_count = contract.hashlock_script.instructions().count();
        if hashlock_instruction_count != 5 {
            return Err(TakerError::General(format!(
                "Maker {} Taproot hashlock script has {} instructions, expected 5",
                maker_idx, hashlock_instruction_count
            )));
        }

        // Verify timelock script has expected format (5 instructions):
        // <locktime> OP_CLTV OP_DROP <pubkey> OP_CHECKSIG
        let timelock_instruction_count = contract.timelock_script.instructions().count();
        if timelock_instruction_count != 5 {
            return Err(TakerError::General(format!(
                "Maker {} Taproot timelock script has {} instructions, expected 5",
                maker_idx, timelock_instruction_count
            )));
        }

        let maker_locktime_val: u64 = if let Some(first) =
            contract.timelock_script.instructions().next()
        {
            match first.map_err(|e| {
                TakerError::General(format!(
                    "Maker {} Taproot timelock script parse error: {:?}",
                    maker_idx, e
                ))
            })? {
                bitcoin::script::Instruction::PushBytes(locktime_bytes) => {
                    let bytes = locktime_bytes.as_bytes();
                    if bytes.is_empty() {
                        return Err(TakerError::General(format!(
                            "Maker {} Taproot timelock script has empty locktime",
                            maker_idx
                        )));
                    }
                    match bytes.len() {
                        1 => bytes[0] as u64,
                        2 => u16::from_le_bytes([bytes[0], bytes[1]]) as u64,
                        3 => u32::from_le_bytes([bytes[0], bytes[1], bytes[2], 0]) as u64,
                        4 => u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as u64,
                        _ => {
                            return Err(TakerError::General(format!(
                                "Maker {} Taproot timelock has unexpected byte length {}",
                                maker_idx,
                                bytes.len()
                            )));
                        }
                    }
                }
                bitcoin::script::Instruction::Op(opcode) => {
                    if let bitcoin::opcodes::Class::PushNum(n) =
                        opcode.classify(bitcoin::opcodes::ClassifyContext::Legacy)
                    {
                        if n <= 0 {
                            return Err(TakerError::General(format!(
                                "Maker {} Taproot timelock value is non-positive ({})",
                                maker_idx, n
                            )));
                        }
                        n as u64
                    } else {
                        return Err(TakerError::General(format!(
                            "Maker {} Taproot timelock script doesn't start with a locktime",
                            maker_idx
                        )));
                    }
                }
            }
        } else {
            return Err(TakerError::General(format!(
                "Maker {} Taproot timelock script is empty",
                maker_idx
            )));
        };

        if maker_locktime_val == 0 {
            return Err(TakerError::General(format!(
                "Maker {} Taproot timelock value is zero",
                maker_idx
            )));
        }

        // Verify the maker used exactly the absolute locktime we sent in SwapDetails.
        if maker_locktime_val != expected_locktime as u64 {
            return Err(TakerError::General(format!(
                "Maker {} Taproot timelock value {} does not match expected {}",
                maker_idx, maker_locktime_val, expected_locktime
            )));
        }

        // Reconstruct the expected Taproot output scriptpubkey from the verified
        // scripts and the maker's claimed internal key, then check every contract
        // tx output pays to that address.
        let secp = Secp256k1::verification_only();
        let expected_spk = {
            let builder = bitcoin::taproot::TaprootBuilder::new()
                .add_leaf(1, contract.hashlock_script.clone())
                .map_err(|e| {
                    TakerError::General(format!(
                        "Maker {} Taproot tree build failed (hashlock leaf): {:?}",
                        maker_idx, e
                    ))
                })?
                .add_leaf(1, contract.timelock_script.clone())
                .map_err(|e| {
                    TakerError::General(format!(
                        "Maker {} Taproot tree build failed (timelock leaf): {:?}",
                        maker_idx, e
                    ))
                })?;
            let tap_info = builder
                .finalize(&secp, contract.internal_key)
                .map_err(|e| {
                    TakerError::General(format!(
                        "Maker {} Taproot tree finalization failed: {:?}",
                        maker_idx, e
                    ))
                })?;
            bitcoin::ScriptBuf::new_p2tr_tweaked(tap_info.output_key())
        };

        for (i, tx) in contract.contract_txs.iter().enumerate() {
            if tx.input.is_empty() {
                return Err(TakerError::General(format!(
                    "Maker {} Taproot contract tx {} has no inputs",
                    maker_idx, i
                )));
            }
            if tx.output.is_empty() {
                return Err(TakerError::General(format!(
                    "Maker {} Taproot contract tx {} has no outputs",
                    maker_idx, i
                )));
            }
            if tx.output[0].script_pubkey != expected_spk {
                return Err(TakerError::General(format!(
                    "Maker {} Taproot contract tx {} output scriptpubkey does not match \
                     expected P2TR address derived from (internal_key, script_tree)",
                    maker_idx, i
                )));
            }
        }

        // Verify total amount is consistent with expected amount after fees
        if let Some(min_amount) = min_expected_amount {
            let total_amount: bitcoin::Amount = contract.amounts.iter().copied().sum();
            if total_amount < min_amount {
                return Err(TakerError::General(format!(
                    "Maker {} Taproot contract total amount {} is below expected minimum {} \
                     (based on maker's advertised fee schedule)",
                    maker_idx, total_amount, min_amount
                )));
            }
        }

        log::info!(
            "Verified Taproot contract data from maker {}: {} contract txs (hash, timelock, structure, amounts)",
            maker_idx,
            contract.contract_txs.len()
        );
        Ok(())
    }
}

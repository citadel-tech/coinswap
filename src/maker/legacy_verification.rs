//! Legacy (ECDSA) message verification for the Maker.
//!
//! Verifies taker messages received during the Legacy swap flow, mirroring
//! the taker's `legacy_verification.rs` but from the maker's perspective.

use bitcoin::PublicKey;

use crate::{
    protocol::{
        contract::{
            calculate_pubkey_from_nonce, check_multisig_has_pubkey, check_reedemscript_is_multisig,
            create_multisig_redeemscript, is_contract_out_valid, verify_contract_tx_sig,
        },
        legacy_messages::ContractTxInfoForSender,
        Hash160,
    },
    taker::api::REFUND_LOCKTIME_STEP,
    wallet::swapcoin::{IncomingSwapCoin, OutgoingSwapCoin},
};

use super::error::MakerError;

/// Verify sender contract tx details before signing (ReqContractSigsForSender)
#[hotpath::measure]
pub(crate) fn verify_req_contract_sigs_for_sender(
    txs_info: &[ContractTxInfoForSender],
    tweakable_pubkey: &PublicKey,
    hashvalue: &Hash160,
    locktime: u16,
    network_port: u16,
) -> Result<(), MakerError> {
    if txs_info.is_empty() {
        return Err(MakerError::General(
            "Empty sender contract txs info from taker",
        ));
    }

    let taker_locktime = locktime + REFUND_LOCKTIME_STEP;

    for (i, txinfo) in txs_info.iter().enumerate() {
        // Validate multisig redeemscript is a 2-of-2 multisig
        check_reedemscript_is_multisig(&txinfo.multisig_redeemscript).map_err(|e| {
            MakerError::General(
                format!(
                    "Sender contract {} invalid multisig redeemscript: {:?}",
                    i, e
                )
                .leak(),
            )
        })?;

        // Verify maker's derived pubkey is in the multisig
        check_multisig_has_pubkey(
            &txinfo.multisig_redeemscript,
            tweakable_pubkey,
            &txinfo.multisig_nonce,
        )
        .map_err(|e| {
            MakerError::General(
                format!(
                    "Sender contract {} multisig doesn't contain maker's pubkey: {:?}",
                    i, e
                )
                .leak(),
            )
        })?;

        // Validate contract tx structure (1 input, 1 output)
        if txinfo.senders_contract_tx.input.len() != 1
            || txinfo.senders_contract_tx.output.len() != 1
        {
            return Err(MakerError::General(
                format!(
                    "Sender contract tx {} has invalid structure: {} inputs, {} outputs",
                    i,
                    txinfo.senders_contract_tx.input.len(),
                    txinfo.senders_contract_tx.output.len()
                )
                .leak(),
            ));
        }

        // Derive the hashlock pubkey from the nonce and maker's tweakable point
        let hashlock_pubkey = calculate_pubkey_from_nonce(tweakable_pubkey, &txinfo.hashlock_nonce)
            .map_err(|_| {
                MakerError::General(
                    format!("Sender contract {} hashlock key derivation failed", i).leak(),
                )
            })?;

        is_contract_out_valid(
            &txinfo.senders_contract_tx.output[0],
            &hashlock_pubkey,
            &txinfo.timelock_pubkey,
            hashvalue,
            &taker_locktime,
            &0,
        )
        .map_err(|e| {
            MakerError::General(format!("Sender contract {} output invalid: {:?}", i, e).leak())
        })?;
    }

    log::info!(
        "[{}] Verified {} sender contract txs (multisig, pubkeys, structure, P2WSH output)",
        network_port,
        txs_info.len()
    );
    Ok(())
}

/// Verify taker's contract signatures (RespContractSigsForRecvrAndSender).
#[hotpath::measure]
pub(crate) fn verify_contract_sigs(
    receivers_sigs: &[bitcoin::ecdsa::Signature],
    senders_sigs: &[bitcoin::ecdsa::Signature],
    incoming_swapcoins: &[IncomingSwapCoin],
    outgoing_swapcoins: &[OutgoingSwapCoin],
    network_port: u16,
) -> Result<(), MakerError> {
    for (i, (sig, incoming)) in receivers_sigs
        .iter()
        .zip(incoming_swapcoins.iter())
        .enumerate()
    {
        // The other_pubkey is the taker's pubkey that should have signed this contract
        let other_pubkey = incoming.other_pubkey.ok_or(MakerError::General(
            "Incoming swapcoin missing other_pubkey for signature verification",
        ))?;

        // Build the multisig redeemscript to compute the correct sighash
        let my_pubkey = incoming
            .my_pubkey
            .ok_or(MakerError::General("Incoming swapcoin missing my_pubkey"))?;
        let multisig_redeemscript = create_multisig_redeemscript(&my_pubkey, &other_pubkey);

        verify_contract_tx_sig(
            &incoming.contract_tx,
            &multisig_redeemscript,
            incoming.funding_amount,
            &other_pubkey,
            &sig.signature,
        )
        .map_err(|e| {
            MakerError::General(
                format!("Receiver signature {} verification failed: {:?}", i, e).leak(),
            )
        })?;
    }

    // Verify sender signatures on outgoing swapcoins
    for (i, (sig, outgoing)) in senders_sigs
        .iter()
        .zip(outgoing_swapcoins.iter())
        .enumerate()
    {
        let other_pubkey = outgoing.other_pubkey.ok_or(MakerError::General(
            "Outgoing swapcoin missing other_pubkey for signature verification",
        ))?;

        let my_pubkey = outgoing
            .my_pubkey
            .ok_or(MakerError::General("Outgoing swapcoin missing my_pubkey"))?;
        let multisig_redeemscript = create_multisig_redeemscript(&my_pubkey, &other_pubkey);

        verify_contract_tx_sig(
            &outgoing.contract_tx,
            &multisig_redeemscript,
            outgoing.funding_amount,
            &other_pubkey,
            &sig.signature,
        )
        .map_err(|e| {
            MakerError::General(
                format!("Sender signature {} verification failed: {:?}", i, e).leak(),
            )
        })?;
    }

    log::info!(
        "[{}] Verified {} receiver + {} sender contract signatures",
        network_port,
        receivers_sigs.len(),
        senders_sigs.len()
    );
    Ok(())
}

/// Verify Legacy private key handover from taker.
#[hotpath::measure]
pub(crate) fn verify_legacy_privkey_handover(
    privkeys: &[crate::protocol::common_messages::SwapPrivkey],
    incoming_swapcoins: &[IncomingSwapCoin],
    network_port: u16,
) -> Result<(), MakerError> {
    if privkeys.len() != incoming_swapcoins.len() {
        return Err(MakerError::General(
            format!(
                "Privkey count {} doesn't match incoming swapcoin count {}",
                privkeys.len(),
                incoming_swapcoins.len()
            )
            .leak(),
        ));
    }

    let secp = bitcoin::secp256k1::Secp256k1::new();

    for (privkey_info, incoming) in privkeys.iter().zip(incoming_swapcoins.iter()) {
        // Derive the public key from the received private key
        let derived_pubkey = PublicKey {
            compressed: true,
            inner: bitcoin::secp256k1::PublicKey::from_secret_key(&secp, &privkey_info.key),
        };

        // Verify it matches the expected other_pubkey on the incoming swapcoin
        let expected_pubkey = incoming.other_pubkey.ok_or(MakerError::General(
            "Missing other_pubkey on incoming swapcoin during handover verification",
        ))?;
        if derived_pubkey != expected_pubkey {
            return Err(MakerError::General(
                "Privkey derives pubkey that doesn't match expected other_pubkey",
            ));
        }
    }

    log::info!(
        "[{}] Verified {} Legacy private keys (derived pubkey matches expected)",
        network_port,
        privkeys.len()
    );
    Ok(())
}

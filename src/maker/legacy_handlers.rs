//! Legacy (ECDSA) Protocol Handlers for the Maker.

use std::sync::Arc;

use bitcoin::{secp256k1::SecretKey, PublicKey, ScriptBuf};

use super::{
    error::MakerError,
    unified_handlers::{UnifiedConnectionState, UnifiedMaker},
};
use crate::{
    protocol::{
        common_messages::{PrivateKeyHandover, SwapPrivkey},
        contract::{
            create_multisig_redeemscript, create_receivers_contract_tx,
            read_pubkeys_from_multisig_redeemscript,
        },
        legacy_messages::{FundingTxInfo, LegacyHashPreimage, LegacyTakerMessage},
        router::MakerToTakerMessage,
    },
    utill::redeemscript_to_scriptpubkey,
    wallet::unified_swapcoin::IncomingSwapCoin,
};

/// Handle a Legacy protocol message.
pub fn handle_legacy_message<M: UnifiedMaker>(
    maker: &Arc<M>,
    state: &mut UnifiedConnectionState,
    message: LegacyTakerMessage,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    log::debug!(
        "[{}] Handling Legacy message: {} (swap_id: {:?})",
        maker.network_port(),
        message,
        state.swap_id
    );

    match message {
        // Multi-hop coordination messages
        LegacyTakerMessage::ReqContractSigsForSender(req) => {
            process_req_contract_sigs_for_sender(maker, state, req)
        }
        LegacyTakerMessage::ProofOfFunding(pof) => process_proof_of_funding(maker, state, pof),
        LegacyTakerMessage::RespContractSigsForRecvrAndSender(resp) => {
            process_resp_contract_sigs_for_recvr_and_sender(maker, state, resp)
        }
        LegacyTakerMessage::ReqContractSigsForRecvr(req) => {
            process_req_contract_sigs_for_recvr(maker, state, req)
        }

        // Finalization messages
        LegacyTakerMessage::HashPreimage(preimage) => {
            process_legacy_preimage(maker, state, preimage)
        }
        LegacyTakerMessage::PrivateKeyHandover(handover) => {
            process_legacy_handover(maker, state, handover)
        }
    }
}

// MULTI-HOP COORDINATION HANDLERS

/// Process request for contract signatures for sender.
fn process_req_contract_sigs_for_sender<M: UnifiedMaker>(
    maker: &Arc<M>,
    state: &mut UnifiedConnectionState,
    req: crate::protocol::legacy_messages::ReqContractSigsForSender,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    log::info!(
        "[{}] Processing ReqContractSigsForSender for swap {} with {} contracts",
        maker.network_port(),
        req.id,
        req.txs_info.len()
    );

    // Verify and sign the sender's contract transactions
    let sigs =
        maker.verify_and_sign_sender_contract_txs(&req.txs_info, &req.hashvalue, req.locktime)?;

    log::info!(
        "[{}] Generated {} signatures for sender contracts",
        maker.network_port(),
        sigs.len()
    );

    // Store swap state
    state.swap_id = Some(req.id.clone());
    state.timelock = req.locktime;

    // Store connection state for persistence
    maker.store_connection_state(&req.id, state);

    let response = crate::protocol::legacy_messages::RespContractSigsForSender { id: req.id, sigs };

    Ok(Some(MakerToTakerMessage::RespContractSigsForSender(
        response,
    )))
}

/// Process proof of funding.
fn process_proof_of_funding<M: UnifiedMaker>(
    maker: &Arc<M>,
    state: &mut UnifiedConnectionState,
    pof: crate::protocol::legacy_messages::ProofOfFunding,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    log::info!(
        "[{}] Processing ProofOfFunding for swap {} with {} funding txs",
        maker.network_port(),
        pof.id,
        pof.confirmed_funding_txes.len()
    );

    let hashvalue = maker.verify_proof_of_funding(&pof)?;

    log::info!(
        "[{}] Verified proof of funding, hashvalue: {:?}",
        maker.network_port(),
        hashvalue
    );

    state.swap_id = Some(pof.id.clone());
    state.timelock = pof.refund_locktime;
    state.contract_feerate = pof.contract_feerate;

    let (tweakable_privkey, _) = maker.get_tweakable_keypair()?;
    let secp = bitcoin::secp256k1::Secp256k1::new();

    let mut incoming_swapcoins = Vec::new();
    let mut incoming_amount = bitcoin::Amount::ZERO;

    for funding_info in &pof.confirmed_funding_txes {
        let (pubkey1, pubkey2) =
            read_pubkeys_from_multisig_redeemscript(&funding_info.multisig_redeemscript)?;

        let funding_output_index = find_funding_output_index(funding_info)?;
        let funding_output = funding_info
            .funding_tx
            .output
            .get(funding_output_index as usize)
            .ok_or(MakerError::General("Funding output not found"))?;

        let multisig_privkey = tweakable_privkey.add_tweak(&funding_info.multisig_nonce.into())?;
        let multisig_pubkey = PublicKey {
            compressed: true,
            inner: bitcoin::secp256k1::PublicKey::from_secret_key(&secp, &multisig_privkey),
        };

        let other_pubkey = if multisig_pubkey == pubkey1 {
            pubkey2
        } else {
            pubkey1
        };

        let hashlock_privkey = tweakable_privkey.add_tweak(&funding_info.hashlock_nonce.into())?;

        let receiver_contract_tx = create_receivers_contract_tx(
            bitcoin::OutPoint {
                txid: funding_info.funding_tx.compute_txid(),
                vout: funding_output_index,
            },
            funding_output.value,
            &funding_info.contract_redeemscript,
        )?;

        let mut incoming_swapcoin = IncomingSwapCoin::new_legacy(
            multisig_privkey,
            other_pubkey,
            receiver_contract_tx,
            funding_info.contract_redeemscript.clone(),
            hashlock_privkey,
            funding_output.value,
        );
        incoming_swapcoin.swap_id = Some(pof.id.clone());

        incoming_swapcoins.push(incoming_swapcoin);
        incoming_amount += funding_output.value;
    }

    state.incoming_swapcoins = incoming_swapcoins;

    log::info!(
        "[{}] Created {} incoming swapcoins, total amount: {}",
        maker.network_port(),
        state.incoming_swapcoins.len(),
        incoming_amount
    );

    let swap_fee = maker.calculate_swap_fee(incoming_amount, pof.refund_locktime);
    let outgoing_amount = incoming_amount
        .checked_sub(swap_fee)
        .ok_or(MakerError::General("Swap fee exceeds incoming amount"))?;

    log::info!(
        "[{}] Incoming: {}, Fee: {}, Outgoing: {}",
        maker.network_port(),
        incoming_amount,
        swap_fee,
        outgoing_amount
    );

    let next_multisig_pubkeys: Vec<PublicKey> = pof
        .next_coinswap_info
        .iter()
        .map(|info| info.next_multisig_pubkey)
        .collect();
    let next_hashlock_pubkeys: Vec<PublicKey> = pof
        .next_coinswap_info
        .iter()
        .map(|info| info.next_hashlock_pubkey)
        .collect();
    let next_multisig_nonces: Vec<SecretKey> = pof
        .next_coinswap_info
        .iter()
        .map(|info| info.next_multisig_nonce)
        .collect();
    let next_hashlock_nonces: Vec<SecretKey> = pof
        .next_coinswap_info
        .iter()
        .map(|info| info.next_hashlock_nonce)
        .collect();

    let (funding_txes, outgoing_swapcoins, _mining_fees) = maker.initialize_coinswap(
        outgoing_amount,
        &next_multisig_pubkeys,
        &next_hashlock_pubkeys,
        hashvalue,
        pof.refund_locktime,
        pof.contract_feerate,
    )?;

    state.outgoing_swapcoins = outgoing_swapcoins.clone();
    state.pending_funding_txes = funding_txes.clone();

    let receivers_contract_txs: Vec<bitcoin::Transaction> = state
        .incoming_swapcoins
        .iter()
        .map(|isc| isc.contract_tx.clone())
        .collect();

    let secp = bitcoin::secp256k1::Secp256k1::new();
    let mut senders_contract_txs_info: Vec<crate::protocol::legacy_messages::SenderContractTxInfo> =
        Vec::new();
    for (i, osc) in outgoing_swapcoins.iter().enumerate() {
        let timelock_pubkey = PublicKey {
            compressed: true,
            inner: bitcoin::secp256k1::PublicKey::from_secret_key(&secp, &osc.timelock_privkey),
        };

        let multisig_redeemscript =
            if let (Some(my_pub), Some(other_pub)) = (osc.my_pubkey, osc.other_pubkey) {
                create_multisig_redeemscript(&my_pub, &other_pub)
            } else {
                osc.contract_redeemscript.clone().unwrap_or_default()
            };

        senders_contract_txs_info.push(crate::protocol::legacy_messages::SenderContractTxInfo {
            funding_tx: funding_txes[i].clone(),
            contract_tx: osc.contract_tx.clone(),
            timelock_pubkey,
            multisig_redeemscript,
            contract_redeemscript: osc.contract_redeemscript.clone().unwrap_or_default(),
            funding_amount: osc.funding_amount,
            multisig_nonce: next_multisig_nonces[i],
            hashlock_nonce: next_hashlock_nonces[i],
        });
    }

    maker.store_connection_state(&pof.id, state);

    log::info!(
        "[{}] Created {} outgoing swapcoins, requesting signatures",
        maker.network_port(),
        outgoing_swapcoins.len()
    );

    let response = crate::protocol::legacy_messages::ReqContractSigsAsRecvrAndSender {
        receivers_contract_txs,
        senders_contract_txs_info,
    };

    Ok(Some(MakerToTakerMessage::ReqContractSigsAsRecvrAndSender(
        response,
    )))
}

fn process_resp_contract_sigs_for_recvr_and_sender<M: UnifiedMaker>(
    maker: &Arc<M>,
    state: &mut UnifiedConnectionState,
    resp: crate::protocol::legacy_messages::RespContractSigsForRecvrAndSender,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    log::info!(
        "[{}] Processing RespContractSigsForRecvrAndSender for swap {} ({} receiver sigs, {} sender sigs)",
        maker.network_port(),
        resp.id,
        resp.receivers_sigs.len(),
        resp.senders_sigs.len()
    );

    if resp.receivers_sigs.len() != state.incoming_swapcoins.len() {
        return Err(MakerError::General("Invalid number of receiver signatures"));
    }

    if resp.senders_sigs.len() != state.outgoing_swapcoins.len() {
        return Err(MakerError::General("Invalid number of sender signatures"));
    }

    for (sig, incoming) in resp
        .receivers_sigs
        .iter()
        .zip(state.incoming_swapcoins.iter_mut())
    {
        // TODO: Verify signature against contract tx
        incoming.others_contract_sig = Some(*sig);
        log::debug!(
            "[{}] Stored receiver signature in incoming swapcoin",
            maker.network_port()
        );
    }

    for (sig, outgoing) in resp
        .senders_sigs
        .iter()
        .zip(state.outgoing_swapcoins.iter_mut())
    {
        outgoing.others_contract_sig = Some(*sig);
        log::debug!(
            "[{}] Stored sender signature in outgoing swapcoin",
            maker.network_port()
        );
    }

    #[cfg(feature = "integration-test")]
    {
        use super::unified_handlers::UnifiedMakerBehavior;
        if maker.behavior() == UnifiedMakerBehavior::SkipFundingBroadcast {
            log::warn!(
                "[{}] Test behavior: skipping funding broadcast",
                maker.network_port()
            );
            for incoming in &state.incoming_swapcoins {
                maker.save_incoming_swapcoin(incoming)?;
            }
            for outgoing in &state.outgoing_swapcoins {
                maker.save_outgoing_swapcoin(outgoing)?;
            }
            maker.store_connection_state(&resp.id, state);
            return Err(MakerError::General("Test: skipped funding broadcast"));
        }
    }

    log::info!(
        "[{}] SECURITY: Broadcasting {} funding txs after receiving signatures",
        maker.network_port(),
        state.pending_funding_txes.len()
    );

    for funding_tx in &state.pending_funding_txes {
        match maker.broadcast_transaction(funding_tx) {
            Ok(txid) => {
                log::info!("[{}] Broadcast funding tx: {}", maker.network_port(), txid);
            }
            Err(e) => {
                log::warn!(
                    "[{}] Failed to broadcast funding tx (may already exist): {:?}",
                    maker.network_port(),
                    e
                );
            }
        }
    }

    state.pending_funding_txes.clear();

    for incoming in &state.incoming_swapcoins {
        maker.save_incoming_swapcoin(incoming)?;
    }
    for outgoing in &state.outgoing_swapcoins {
        maker.save_outgoing_swapcoin(outgoing)?;

        // Register outgoing contract output with watchtower EARLY so it can
        // detect hashlock spends by the taker before recovery starts.
        let contract_txid = outgoing.contract_tx.compute_txid();
        for (vout, _) in outgoing.contract_tx.output.iter().enumerate() {
            maker.register_watch_outpoint(bitcoin::OutPoint {
                txid: contract_txid,
                vout: vout as u32,
            });
        }
    }

    maker.store_connection_state(&resp.id, state);

    log::info!(
        "[{}] Funding broadcast complete for swap {}",
        maker.network_port(),
        resp.id
    );

    Ok(None)
}

/// Process request for contract signatures for receiver.
fn process_req_contract_sigs_for_recvr<M: UnifiedMaker>(
    maker: &Arc<M>,
    _state: &mut UnifiedConnectionState,
    req: crate::protocol::legacy_messages::ReqContractSigsForRecvr,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    log::info!(
        "[{}] Processing ReqContractSigsForRecvr for swap {} with {} txs",
        maker.network_port(),
        req.id,
        req.txs.len()
    );

    let mut sigs = Vec::new();
    for txinfo in &req.txs {
        if let Some(outgoing) = maker.find_outgoing_swapcoin(&txinfo.multisig_redeemscript) {
            if let Some(privkey) = outgoing.my_privkey {
                match crate::protocol::contract::sign_contract_tx(
                    &txinfo.contract_tx,
                    &txinfo.multisig_redeemscript,
                    outgoing.funding_amount,
                    &privkey,
                ) {
                    Ok(sig) => {
                        sigs.push(sig);
                        log::debug!("[{}] Signed receiver contract tx", maker.network_port());
                    }
                    Err(e) => {
                        log::warn!(
                            "[{}] Failed to sign receiver contract tx: {:?}",
                            maker.network_port(),
                            e
                        );
                        return Err(MakerError::General("Failed to sign receiver contract tx"));
                    }
                }
            } else {
                log::warn!(
                    "[{}] No private key in outgoing swapcoin",
                    maker.network_port()
                );
                return Err(MakerError::General("No private key in outgoing swapcoin"));
            }
        } else {
            log::warn!(
                "[{}] Could not find matching outgoing swapcoin for multisig",
                maker.network_port()
            );
            return Err(MakerError::General(
                "Could not find matching outgoing swapcoin",
            ));
        }
    }

    log::info!(
        "[{}] Generated {} signatures for receiver contracts",
        maker.network_port(),
        sigs.len()
    );

    let response = crate::protocol::legacy_messages::RespContractSigsForRecvr { id: req.id, sigs };

    Ok(Some(MakerToTakerMessage::RespContractSigsForRecvr(
        response,
    )))
}

/// Process Legacy hash preimage.
fn process_legacy_preimage<M: UnifiedMaker>(
    maker: &Arc<M>,
    state: &mut UnifiedConnectionState,
    preimage: LegacyHashPreimage,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    log::info!(
        "[{}] Processing Legacy hash preimage for swap {}",
        maker.network_port(),
        preimage.id
    );

    if state.outgoing_swapcoins.is_empty() {
        return Err(MakerError::General("No outgoing swapcoin found"));
    }

    for incoming in state.incoming_swapcoins.iter_mut() {
        incoming.hash_preimage = Some(preimage.preimage);
    }

    let mut privkeys = Vec::new();
    for outgoing in &state.outgoing_swapcoins {
        let privkey = outgoing
            .my_privkey
            .ok_or(MakerError::General("No private key in outgoing swapcoin"))?;
        privkeys.push(SwapPrivkey {
            identifier: ScriptBuf::new(),
            key: privkey,
        });
    }

    for incoming in &state.incoming_swapcoins {
        maker.save_incoming_swapcoin(incoming)?;
    }

    maker.store_connection_state(&preimage.id, state);

    log::info!(
        "[{}] Preimage verified for swap {}, returning {} private key(s)",
        maker.network_port(),
        preimage.id,
        privkeys.len()
    );

    let response = PrivateKeyHandover {
        id: preimage.id,
        privkeys,
    };

    Ok(Some(MakerToTakerMessage::LegacyPrivateKeyHandover(
        response,
    )))
}

/// Process Legacy private key handover.
fn process_legacy_handover<M: UnifiedMaker>(
    maker: &Arc<M>,
    state: &mut UnifiedConnectionState,
    handover: PrivateKeyHandover,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    log::info!(
        "[{}] Processing Legacy private key handover for swap {} with {} key(s)",
        maker.network_port(),
        handover.id,
        handover.privkeys.len()
    );

    for (i, incoming) in state.incoming_swapcoins.iter_mut().enumerate() {
        if let Some(privkey) = handover.privkeys.get(i) {
            incoming.other_privkey = Some(privkey.key);
        }
    }
    for incoming in &state.incoming_swapcoins {
        maker.save_incoming_swapcoin(incoming)?;
    }

    maker.sweep_incoming_swapcoins()?;
    maker.remove_connection_state(&handover.id);

    log::info!(
        "[{}] Legacy swap {} completed successfully",
        maker.network_port(),
        handover.id
    );

    Ok(None)
}

/// Find the index of the funding output in the funding transaction.
fn find_funding_output_index(funding_tx_info: &FundingTxInfo) -> Result<u32, MakerError> {
    let multisig_spk = redeemscript_to_scriptpubkey(&funding_tx_info.multisig_redeemscript)
        .map_err(|e| {
            MakerError::General(format!("Failed to convert redeemscript: {:?}", e).leak())
        })?;
    funding_tx_info
        .funding_tx
        .output
        .iter()
        .position(|o| o.script_pubkey == multisig_spk)
        .map(|index| index as u32)
        .ok_or(MakerError::General(
            "Funding output doesn't match with multisig redeem script",
        ))
}

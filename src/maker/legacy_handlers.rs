//! Legacy (ECDSA) Protocol Handlers for the Maker.

use std::sync::Arc;

use bitcoin::{secp256k1::SecretKey, PublicKey, ScriptBuf};

use super::{
    error::MakerError,
    unified_handlers::{SwapPhase, UnifiedConnectionState, UnifiedMaker},
};
use crate::{
    protocol::{
        common_messages::{PrivateKeyHandover, SwapPrivkey},
        contract::{
            create_multisig_redeemscript, create_receivers_contract_tx,
            read_pubkeys_from_multisig_redeemscript,
        },
        legacy_messages::{FundingTxInfo, LegacyTakerMessage},
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
    // Allow re-processing in AwaitingSignaturesOrPreimage: when the taker retries
    // after substituting a failed next-hop maker, it reconnects and re-sends this
    // message. The re-signing is safe since no funding has been broadcast yet.
    state.expect_phase(&[
        SwapPhase::AwaitingContractData,
        SwapPhase::AwaitingSignaturesOrPreimage,
    ])?;
    state.check_swap_id(&req.id)?;

    #[cfg(feature = "integration-test")]
    {
        use super::unified_handlers::UnifiedMakerBehavior;
        if maker.behavior() == UnifiedMakerBehavior::CloseAtReqContractSigsForSender {
            log::warn!(
                "[{}] Test behavior: closing at ReqContractSigsForSender",
                maker.network_port()
            );
            return Err(MakerError::General(
                "Test: closing at ReqContractSigsForSender",
            ));
        }
    }

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
    // Allow re-processing in AwaitingSignaturesOrPreimage: when the taker retries
    // after substituting a failed next-hop maker, it reconnects and re-sends
    // ProofOfFunding so the maker creates new outgoing swapcoins with the spare's keys.
    // This is safe since no funding has been broadcast yet at this stage.
    state.expect_phase(&[
        SwapPhase::AwaitingContractData,
        SwapPhase::AwaitingSignaturesOrPreimage,
    ])?;
    state.check_swap_id(&pof.id)?;

    #[cfg(feature = "integration-test")]
    {
        use super::unified_handlers::UnifiedMakerBehavior;
        if maker.behavior() == UnifiedMakerBehavior::CloseAtProofOfFunding {
            log::warn!(
                "[{}] Test behavior: closing at ProofOfFunding",
                maker.network_port()
            );
            return Err(MakerError::General("Test: closing at ProofOfFunding"));
        }
    }

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

    // Register incoming contract outputs with watchtower so we detect
    // if the taker broadcasts the maker's incoming contract tx.
    for incoming in &state.incoming_swapcoins {
        let txid = incoming.contract_tx.compute_txid();
        for (vout, _) in incoming.contract_tx.output.iter().enumerate() {
            maker.register_watch_outpoint(bitcoin::OutPoint {
                txid,
                vout: vout as u32,
            });
        }
    }

    log::info!(
        "[{}] Created {} incoming swapcoins, total amount: {}",
        maker.network_port(),
        state.incoming_swapcoins.len(),
        incoming_amount
    );

    let swap_fee = maker.calculate_swap_fee(incoming_amount, pof.refund_locktime as u32);
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

    // Sync wallet before creating outgoing swaps to get fresh UTXO state.
    log::info!(
        "[{}] Sync at:----process_proof_of_funding----",
        maker.network_port()
    );
    maker.sync_and_save_wallet()?;

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

    state.phase = SwapPhase::AwaitingSignaturesOrPreimage;
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
    state.expect_phase(&[SwapPhase::AwaitingSignaturesOrPreimage])?;
    state.check_swap_id(&resp.id)?;

    #[cfg(feature = "integration-test")]
    {
        use super::unified_handlers::UnifiedMakerBehavior;
        if maker.behavior() == UnifiedMakerBehavior::CloseAtContractSigsForRecvrAndSender {
            log::warn!(
                "[{}] Test behavior: closing at RespContractSigsForRecvrAndSender",
                maker.network_port()
            );
            return Err(MakerError::General(
                "Test: closing at ContractSigsForRecvrAndSender",
            ));
        }
    }

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

    // Verify all contract signatures before storing them
    super::legacy_verification::verify_contract_sigs(
        &resp.receivers_sigs,
        &resp.senders_sigs,
        &state.incoming_swapcoins,
        &state.outgoing_swapcoins,
        maker.network_port(),
    )?;

    for (sig, incoming) in resp
        .receivers_sigs
        .iter()
        .zip(state.incoming_swapcoins.iter_mut())
    {
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
            state.phase = SwapPhase::AwaitingPrivateKeyHandover;
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
    state.funding_broadcast = true;
    state.phase = SwapPhase::AwaitingPrivateKeyHandover;

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

    #[cfg(feature = "integration-test")]
    {
        use super::unified_handlers::UnifiedMakerBehavior;
        if maker.behavior() == UnifiedMakerBehavior::BroadcastContractAfterSetup {
            log::warn!(
                "[{}] Test behavior: broadcasting contract txs after setup, then closing",
                maker.network_port()
            );
            for outgoing in &state.outgoing_swapcoins {
                let _ = maker.broadcast_transaction(&outgoing.contract_tx);
            }
            // Remove stored state so taker can't reconnect and complete the swap
            maker.remove_connection_state(&resp.id);
            return Err(MakerError::General("Test: broadcast contract after setup"));
        }
    }

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
    state: &mut UnifiedConnectionState,
    req: crate::protocol::legacy_messages::ReqContractSigsForRecvr,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    state.expect_phase(&[SwapPhase::AwaitingPrivateKeyHandover])?;
    state.check_swap_id(&req.id)?;

    #[cfg(feature = "integration-test")]
    {
        use super::unified_handlers::UnifiedMakerBehavior;
        if maker.behavior() == UnifiedMakerBehavior::CloseAtContractSigsForRecvr {
            log::warn!(
                "[{}] Test behavior: closing at ReqContractSigsForRecvr",
                maker.network_port()
            );
            return Err(MakerError::General("Test: closing at ContractSigsForRecvr"));
        }
    }

    log::info!(
        "[{}] Processing ReqContractSigsForRecvr for swap {} with {} txs",
        maker.network_port(),
        req.id,
        req.txs.len()
    );

    let mut sigs = Vec::new();
    for (i, txinfo) in req.txs.iter().enumerate() {
        // Validate contract tx structure before signing
        if txinfo.contract_tx.input.len() != 1 || txinfo.contract_tx.output.len() != 1 {
            return Err(MakerError::General(
                format!(
                    "Receiver contract tx {} has invalid structure: {} inputs, {} outputs",
                    i,
                    txinfo.contract_tx.input.len(),
                    txinfo.contract_tx.output.len()
                )
                .leak(),
            ));
        }

        if let Some(outgoing) = maker.find_outgoing_swapcoin(&txinfo.multisig_redeemscript) {
            // Verify the contract tx spends from our funding tx
            if let Some(ref funding_tx) = outgoing.funding_tx {
                let expected_txid = funding_tx.compute_txid();
                let actual_txid = txinfo.contract_tx.input[0].previous_output.txid;
                if actual_txid != expected_txid {
                    return Err(MakerError::General(
                        format!(
                            "Receiver contract tx {} spends from {} but expected {}",
                            i, actual_txid, expected_txid
                        )
                        .leak(),
                    ));
                }
            }

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

/// Process Legacy private key handover.
/// Stores the received privkey on incoming swapcoins, extracts outgoing privkeys
/// as a response, then sweeps.
fn process_legacy_handover<M: UnifiedMaker>(
    maker: &Arc<M>,
    state: &mut UnifiedConnectionState,
    handover: PrivateKeyHandover,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    state.expect_phase(&[SwapPhase::AwaitingPrivateKeyHandover])?;
    state.check_swap_id(&handover.id)?;

    #[cfg(feature = "integration-test")]
    {
        use super::unified_handlers::UnifiedMakerBehavior;
        if maker.behavior() == UnifiedMakerBehavior::CloseAtHashPreimage {
            log::warn!(
                "[{}] Test behavior: closing at hash preimage / private key handover",
                maker.network_port()
            );
            return Err(MakerError::General("Test: closing at hash preimage"));
        }
    }

    log::info!(
        "[{}] Processing Legacy private key handover for swap {} with {} key(s)",
        maker.network_port(),
        handover.id,
        handover.privkeys.len()
    );

    if state.outgoing_swapcoins.is_empty() {
        return Err(MakerError::General("No outgoing swapcoin found"));
    }

    // Verify the received private keys before proceeding
    super::legacy_verification::verify_legacy_privkey_handover(
        &handover.privkeys,
        &state.incoming_swapcoins,
        maker.network_port(),
    )?;

    // Extract outgoing privkeys for response
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

    // Store received privkey on incoming swapcoins
    for (i, incoming) in state.incoming_swapcoins.iter_mut().enumerate() {
        if let Some(privkey) = handover.privkeys.get(i) {
            incoming.other_privkey = Some(privkey.key);
        }
    }
    for incoming in &state.incoming_swapcoins {
        maker.save_incoming_swapcoin(incoming)?;
    }

    // Mark swap as completed — sweep happens in the server loop after the
    // response is delivered to the taker.
    state.phase = SwapPhase::Completed;

    log::info!(
        "[{}] Legacy swap {} completed successfully, returning {} private key(s)",
        maker.network_port(),
        handover.id,
        privkeys.len()
    );

    let response = PrivateKeyHandover {
        id: handover.id,
        privkeys,
    };

    Ok(Some(MakerToTakerMessage::LegacyPrivateKeyHandover(
        response,
    )))
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

//! Taproot (MuSig2) Protocol Handlers for the Maker.

use std::sync::Arc;

use bitcoin::{OutPoint, PublicKey};

use super::{
    error::MakerError,
    unified_handlers::{SwapPhase, UnifiedConnectionState, UnifiedMaker},
};
use crate::{
    protocol::{
        common_messages::{PrivateKeyHandover, SwapPrivkey},
        contract::calculate_pubkey_from_nonce,
        contract2::{
            check_taproot_hashlock_has_pubkey, create_hashlock_script, create_timelock_script,
            extract_hash_from_hashlock,
        },
        router::MakerToTakerMessage,
        taproot_messages::{SerializableScalar, TaprootContractData, TaprootTakerMessage},
    },
    wallet::unified_swapcoin::{IncomingSwapCoin, OutgoingSwapCoin},
};

/// Handle a Taproot protocol message.
pub fn handle_taproot_message<M: UnifiedMaker>(
    maker: &Arc<M>,
    state: &mut UnifiedConnectionState,
    message: TaprootTakerMessage,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    log::debug!(
        "[{}] Handling Taproot message: {:?} (swap_id: {:?})",
        maker.network_port(),
        message,
        state.swap_id
    );

    match message {
        TaprootTakerMessage::ContractData(data) => process_taproot_contract(maker, state, *data),
        TaprootTakerMessage::PrivateKeyHandover(handover) => {
            process_taproot_handover(maker, state, handover)
        }
    }
}

/// Process Taproot contract data.
fn process_taproot_contract<M: UnifiedMaker>(
    maker: &Arc<M>,
    state: &mut UnifiedConnectionState,
    data: TaprootContractData,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    log::info!(
        "[{}] Processing Taproot contract data for swap {}",
        maker.network_port(),
        data.id
    );

    let (tweakable_privkey, tweakable_pubkey) = maker.get_tweakable_keypair()?;
    let secp = bitcoin::secp256k1::Secp256k1::new();

    // Use the nonce from the received message to reconstruct hashlock_privkey.
    let hashlock_nonce = data.hashlock_nonce.ok_or(MakerError::General(
        "Missing hashlock_nonce in TaprootContractData",
    ))?;
    let hashlock_privkey = tweakable_privkey
        .add_tweak(&hashlock_nonce.into())
        .map_err(|_| MakerError::General("Hashlock key derivation failed"))?;

    // Verify: derived pubkey matches the pubkey in the hashlock script.
    check_taproot_hashlock_has_pubkey(&data.hashlock_script, &tweakable_pubkey, &hashlock_nonce)
        .map_err(|e| MakerError::General(format!("Hashlock pubkey mismatch: {:?}", e).leak()))?;

    let incoming_contract_tx = data
        .contract_txs
        .first()
        .cloned()
        .ok_or(MakerError::General("No contract transaction from taker"))?;

    // Verify the incoming contract tx is on-chain before proceeding.
    let incoming_txid = incoming_contract_tx.compute_txid();
    maker.verify_contract_tx_on_chain(&incoming_txid)?;

    let incoming_funding_amount = data.amounts.first().cloned().unwrap_or(state.swap_amount);

    let other_pubkey = data.pubkeys.first().cloned().unwrap_or(data.next_hop_point);

    let mut incoming_swapcoin = IncomingSwapCoin::new_taproot(
        hashlock_privkey,
        data.hashlock_script.clone(),
        data.timelock_script.clone(),
        incoming_contract_tx,
        incoming_funding_amount,
    );
    incoming_swapcoin.swap_id = Some(data.id.clone());

    incoming_swapcoin.my_privkey = Some(tweakable_privkey);
    incoming_swapcoin.my_pubkey = Some(PublicKey {
        compressed: true,
        inner: bitcoin::secp256k1::PublicKey::from_secret_key(&secp, &tweakable_privkey),
    });
    incoming_swapcoin.other_pubkey = Some(other_pubkey);
    incoming_swapcoin.internal_key = Some(data.internal_key);
    incoming_swapcoin.tap_tweak = Some(data.tap_tweak_scalar());

    // Taproot uses absolute CLTV timelocks, convert to relative for fee calculation
    let current_height = maker.get_current_height()?;
    let relative_timelock = state.timelock.saturating_sub(current_height);
    let fee = maker.calculate_swap_fee(incoming_funding_amount, relative_timelock);
    let outgoing_amount = incoming_funding_amount
        .checked_sub(fee)
        .ok_or(MakerError::General("Fee exceeds incoming amount"))?;

    log::info!(
        "[{}] Fee calculation: incoming={}, fee={}, outgoing={}",
        maker.network_port(),
        incoming_funding_amount,
        fee,
        outgoing_amount
    );

    let outgoing_nonce =
        bitcoin::secp256k1::SecretKey::new(&mut bitcoin::secp256k1::rand::thread_rng());
    let outgoing_privkey = tweakable_privkey
        .add_tweak(&outgoing_nonce.into())
        .map_err(|_| MakerError::General("Outgoing key derivation failed"))?;

    let outgoing_pubkey = PublicKey {
        compressed: true,
        inner: bitcoin::secp256k1::PublicKey::from_secret_key(&secp, &outgoing_privkey),
    };

    let timelock_privkey =
        bitcoin::secp256k1::SecretKey::new(&mut bitcoin::secp256k1::rand::thread_rng());
    let timelock_keypair = bitcoin::secp256k1::Keypair::from_secret_key(&secp, &timelock_privkey);
    let timelock_xonly = bitcoin::secp256k1::XOnlyPublicKey::from_keypair(&timelock_keypair).0;

    let hash = extract_hash_from_hashlock(&data.hashlock_script)
        .map_err(|e| MakerError::General(format!("Invalid hashlock script: {:?}", e).leak()))?;
    // If next_hashlock_nonce is provided, tweak the next hop's pubkey.
    // Otherwise (last maker → taker), use the un-tweaked next_hop_point.
    let next_hop_hashlock_pubkey = if let Some(ref nonce) = data.next_hashlock_nonce {
        calculate_pubkey_from_nonce(&data.next_hop_point, nonce)
            .map_err(|e| MakerError::General(format!("Next hop key derivation: {:?}", e).leak()))?
    } else {
        data.next_hop_point
    };
    let next_hop_xonly = bitcoin::key::XOnlyPublicKey::from(next_hop_hashlock_pubkey.inner);
    let hashlock_script = create_hashlock_script(&hash, &next_hop_xonly);
    let timelock_script = {
        // For Taproot, state.timelock is already an absolute block height
        // (computed by the taker from a single reference height).
        let locktime = bitcoin::absolute::LockTime::from_height(state.timelock)
            .map_err(|_| MakerError::General("Invalid locktime height"))?;
        create_timelock_script(locktime, &timelock_xonly)
    };

    let builder = bitcoin::taproot::TaprootBuilder::new()
        .add_leaf(1, hashlock_script.clone())
        .map_err(|e| MakerError::General(format!("Failed to add hashlock leaf: {:?}", e).leak()))?
        .add_leaf(1, timelock_script.clone())
        .map_err(|e| MakerError::General(format!("Failed to add timelock leaf: {:?}", e).leak()))?;

    let mut ordered_pubkeys = [outgoing_pubkey, data.next_hop_point];
    ordered_pubkeys.sort_by(|a, b| a.inner.serialize().cmp(&b.inner.serialize()));
    let internal_key = crate::protocol::musig_interface::get_aggregated_pubkey_compat(
        ordered_pubkeys[0].inner,
        ordered_pubkeys[1].inner,
    )
    .map_err(|e| {
        MakerError::General(format!("Failed to create aggregated pubkey: {:?}", e).leak())
    })?;

    let tap_info = builder
        .finalize(&secp, internal_key)
        .map_err(|e| MakerError::General(format!("Failed to finalize taproot: {:?}", e).leak()))?;

    let taproot_address = bitcoin::Address::p2tr_tweaked(tap_info.output_key(), maker.network());

    let (contract_tx, output_pos) =
        maker.create_funding_transaction(outgoing_amount, taproot_address.clone())?;
    let contract_txid = contract_tx.compute_txid();

    let contract_outpoint = OutPoint {
        txid: contract_txid,
        vout: output_pos,
    };

    let contract_output_amount = contract_tx.output[output_pos as usize].value;

    let mut outgoing_swapcoin = OutgoingSwapCoin::new_taproot(
        timelock_privkey,
        hashlock_script.clone(),
        timelock_script.clone(),
        contract_tx.clone(),
        contract_output_amount,
    );
    outgoing_swapcoin.swap_id = Some(data.id.clone());
    outgoing_swapcoin.set_taproot_params(
        outgoing_privkey,
        outgoing_pubkey,
        data.next_hop_point,
        internal_key,
        tap_info.tap_tweak().to_scalar(),
    );

    // Store in connection state
    state.incoming_swapcoins.push(incoming_swapcoin.clone());
    state.outgoing_swapcoins.push(outgoing_swapcoin.clone());

    #[cfg(feature = "integration-test")]
    {
        use super::unified_handlers::UnifiedMakerBehavior;
        if maker.behavior() == UnifiedMakerBehavior::SkipFundingBroadcast {
            log::warn!(
                "[{}] Test behavior: skipping Taproot funding broadcast",
                maker.network_port()
            );
            // Swapcoins are already in state.
            // Save them to wallet for recovery detection.
            maker.save_incoming_swapcoin(&incoming_swapcoin)?;
            maker.save_outgoing_swapcoin(&outgoing_swapcoin)?;
            maker.store_connection_state(&data.id, state);
            return Err(MakerError::General("Test: skipped funding broadcast"));
        }
    }

    match maker.broadcast_transaction(&contract_tx) {
        Ok(txid) => {
            log::info!(
                "[{}] Broadcast Taproot contract tx {} for swap {}",
                maker.network_port(),
                txid,
                data.id
            );

            maker.register_watch_outpoint(contract_outpoint);
        }
        Err(e) => {
            log::warn!(
                "[{}] Failed to broadcast Taproot contract tx (may already be broadcast): {:?}",
                maker.network_port(),
                e
            );
        }
    }

    state.funding_broadcast = true;
    maker.register_watch_outpoint(contract_outpoint);

    // Save to wallet
    maker.save_incoming_swapcoin(&incoming_swapcoin)?;
    maker.save_outgoing_swapcoin(&outgoing_swapcoin)?;

    maker.store_connection_state(&data.id, state);

    log::info!(
        "[{}] Created Taproot swapcoins for swap {}. Outgoing amount: {}",
        maker.network_port(),
        data.id,
        outgoing_swapcoin.funding_amount
    );

    let tap_tweak_scalar = tap_info.tap_tweak().to_scalar();
    let response = TaprootContractData::new(
        data.id.clone(),
        vec![outgoing_pubkey],
        tweakable_pubkey,
        internal_key,
        SerializableScalar::from_bytes(tap_tweak_scalar.to_be_bytes().to_vec()),
        hashlock_script,
        timelock_script,
        vec![contract_tx],
        vec![contract_output_amount],
        None, // hashlock_nonce: taker already knows
        None, // next_hashlock_nonce: taker manages all nonces
    );

    Ok(Some(MakerToTakerMessage::TaprootContractData(Box::new(
        response,
    ))))
}

/// Process Taproot private key handover.
/// Stores the received privkey on incoming swapcoins, extracts outgoing privkey
/// as a response. Sweep and state cleanup happen in the server loop after the
/// response has been sent to the taker, to avoid blocking delivery.
fn process_taproot_handover<M: UnifiedMaker>(
    maker: &Arc<M>,
    state: &mut UnifiedConnectionState,
    handover: PrivateKeyHandover,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    log::info!(
        "[{}] Processing Taproot private key handover for swap {}",
        maker.network_port(),
        handover.id
    );

    let outgoing = state
        .outgoing_swapcoins
        .first()
        .ok_or(MakerError::General("No outgoing swapcoin found"))?;

    let privkey = outgoing
        .my_privkey
        .ok_or(MakerError::General("No private key in outgoing swapcoin"))?;

    // Store received privkey on incoming swapcoins
    for (i, incoming) in state.incoming_swapcoins.iter_mut().enumerate() {
        if let Some(pk) = handover.privkeys.get(i) {
            incoming.other_privkey = Some(pk.key);
        }
    }

    for incoming in &state.incoming_swapcoins {
        maker.save_incoming_swapcoin(incoming)?;
    }

    // Mark swap as completed — sweep happens in the server loop after the
    // response is delivered to the taker.
    state.phase = SwapPhase::Completed;

    log::info!(
        "[{}] Taproot swap {} completed successfully, returning private key",
        maker.network_port(),
        handover.id
    );

    let response = PrivateKeyHandover {
        id: handover.id,
        privkeys: vec![SwapPrivkey {
            identifier: bitcoin::ScriptBuf::new(),
            key: privkey,
        }],
    };

    Ok(Some(MakerToTakerMessage::TaprootPrivateKeyHandover(
        response,
    )))
}

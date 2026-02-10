//! Taproot (MuSig2) Protocol Handlers for the Maker.

use std::sync::Arc;

use bitcoin::{Amount, OutPoint, PublicKey, ScriptBuf, Transaction};

use super::{
    error::MakerError,
    unified_handlers::{UnifiedConnectionState, UnifiedMaker},
};
use crate::{
    protocol::{
        common_messages::{PrivateKeyHandover, SwapPrivkey},
        contract2::create_timelock_script,
        router::MakerToTakerMessage,
        taproot_messages::{
            SerializableScalar, TaprootContractData, TaprootHashPreimage, TaprootTakerMessage,
        },
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
        TaprootTakerMessage::HashPreimage(preimage) => {
            process_taproot_preimage(maker, state, preimage)
        }
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

    let hashlock_nonce =
        bitcoin::secp256k1::SecretKey::new(&mut bitcoin::secp256k1::rand::thread_rng());
    let hashlock_privkey = tweakable_privkey
        .add_tweak(&hashlock_nonce.into())
        .map_err(|_| MakerError::General("Hashlock key derivation failed"))?;

    let incoming_contract_tx = data
        .contract_txs
        .first()
        .cloned()
        .ok_or(MakerError::General("No contract transaction from taker"))?;

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

    let fee = maker.calculate_swap_fee(incoming_funding_amount, state.timelock);
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

    let hashlock_script = data.hashlock_script.clone();
    let timelock_script = {
        let locktime = bitcoin::absolute::LockTime::from_height(state.timelock as u32)
            .unwrap_or(bitcoin::absolute::LockTime::ZERO);
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

    let (funding_tx, output_pos) =
        maker.create_funding_transaction(outgoing_amount, taproot_address.clone())?;
    let funding_txid = funding_tx.compute_txid();

    let funding_outpoint = OutPoint {
        txid: funding_txid,
        vout: output_pos,
    };

    let funding_output_amount = funding_tx.output[output_pos as usize].value;

    // Create contract tx
    let contract_tx = Transaction {
        version: bitcoin::transaction::Version::TWO,
        lock_time: bitcoin::absolute::LockTime::ZERO,
        input: vec![bitcoin::TxIn {
            previous_output: funding_outpoint,
            script_sig: ScriptBuf::new(),
            sequence: bitcoin::Sequence::ENABLE_RBF_NO_LOCKTIME,
            witness: bitcoin::Witness::new(),
        }],
        output: vec![bitcoin::TxOut {
            value: funding_output_amount - Amount::from_sat(500), // Deduct fee
            script_pubkey: taproot_address.script_pubkey(),
        }],
    };

    // Create outgoing swapcoin
    let mut outgoing_swapcoin = OutgoingSwapCoin::new_taproot(
        timelock_privkey,
        hashlock_script.clone(),
        timelock_script.clone(),
        contract_tx.clone(),
        funding_output_amount,
    );
    outgoing_swapcoin.swap_id = Some(data.id.clone());
    outgoing_swapcoin.funding_tx = Some(funding_tx.clone());
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

    match maker.broadcast_transaction(&funding_tx) {
        Ok(txid) => {
            log::info!(
                "[{}] Broadcast Taproot funding tx {} immediately for swap {}",
                maker.network_port(),
                txid,
                data.id
            );

            maker.register_watch_outpoint(funding_outpoint);
        }
        Err(e) => {
            log::warn!(
                "[{}] Failed to broadcast Taproot funding tx (may already be broadcast): {:?}",
                maker.network_port(),
                e
            );
        }
    }

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
        vec![funding_tx],
        vec![funding_output_amount],
    );

    Ok(Some(MakerToTakerMessage::TaprootContractData(Box::new(
        response,
    ))))
}

/// Process Taproot hash preimage.
fn process_taproot_preimage<M: UnifiedMaker>(
    maker: &Arc<M>,
    state: &mut UnifiedConnectionState,
    preimage: TaprootHashPreimage,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    log::info!(
        "[{}] Processing Taproot hash preimage for swap {}",
        maker.network_port(),
        preimage.id
    );

    let outgoing = state
        .outgoing_swapcoins
        .first()
        .ok_or(MakerError::General("No outgoing swapcoin found"))?;

    let privkey = outgoing
        .my_privkey
        .ok_or(MakerError::General("No private key in outgoing swapcoin"))?;

    for incoming in state.incoming_swapcoins.iter_mut() {
        incoming.hash_preimage = Some(preimage.preimage);
    }

    for incoming in &state.incoming_swapcoins {
        maker.save_incoming_swapcoin(incoming)?;
    }

    maker.store_connection_state(&preimage.id, state);

    log::info!(
        "[{}] Preimage verified for swap {}, returning private key",
        maker.network_port(),
        preimage.id
    );

    let response = PrivateKeyHandover {
        id: preimage.id,
        privkeys: vec![SwapPrivkey {
            identifier: bitcoin::ScriptBuf::new(),
            key: privkey,
        }],
    };

    Ok(Some(MakerToTakerMessage::TaprootPrivateKeyHandover(
        response,
    )))
}

/// Process Taproot private key handover.
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

    for (i, incoming) in state.incoming_swapcoins.iter_mut().enumerate() {
        if let Some(privkey) = handover.privkeys.get(i) {
            incoming.other_privkey = Some(privkey.key);
        }
    }

    for incoming in &state.incoming_swapcoins {
        maker.save_incoming_swapcoin(incoming)?;
    }

    maker.sweep_incoming_swapcoins()?;

    log::info!(
        "[{}] Taproot swap {} completed successfully",
        maker.network_port(),
        handover.id
    );

    Ok(None)
}

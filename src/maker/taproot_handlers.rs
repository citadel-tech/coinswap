//! Taproot (MuSig2) Protocol Handlers for the Maker.

use std::sync::Arc;

use bitcoin::{Amount, OutPoint, PublicKey};
use bitcoind::bitcoincore_rpc::{jsonrpc::error::Error as JsonRpcError, Error as BitcoinRpcError};

use super::{
    error::MakerError,
    handlers::{ConnectionState, Maker, SwapPhase},
};
use crate::{
    protocol::{
        common_messages::{MakerToTakerMessage, PrivateKeyHandover, SwapPrivkey},
        contract::calculate_pubkey_from_nonce,
        contract2::{
            check_taproot_hashlock_has_pubkey, create_hashlock_script, create_timelock_script,
            extract_hash_from_hashlock,
        },
        taproot_messages::{SerializableScalar, TaprootContractData, TaprootTakerMessage},
    },
    utill::estimate_funding_tx_fee_sats,
    wallet::{
        swapcoin::{IncomingSwapCoin, OutgoingSwapCoin},
        MakerReport, WalletError,
    },
};

/// Handle a Taproot protocol message.
#[hotpath::measure]
pub fn handle_taproot_message<M: Maker>(
    maker: &Arc<M>,
    state: &mut ConnectionState,
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
#[hotpath::measure]
fn process_taproot_contract<M: Maker>(
    maker: &Arc<M>,
    state: &mut ConnectionState,
    data: TaprootContractData,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    state.expect_phase(&[SwapPhase::AwaitingContractData])?;
    state.check_swap_id(&data.id)?;

    #[cfg(feature = "integration-test")]
    {
        use super::handlers::MakerBehavior;
        if maker.behavior() == MakerBehavior::CloseAtContractSigsExchange {
            log::warn!(
                "[{}] Test behavior: closing at taproot contract sigs exchange",
                maker.network_port()
            );
            return Err(MakerError::General(
                "Test: closing at contract sigs exchange",
            ));
        }
    }

    log::info!(
        "[{}] Processing Taproot contract data for swap {}",
        maker.network_port(),
        data.id
    );

    let (tweakable_privkey, tweakable_pubkey, _) = maker.get_tweakable_keypair()?;
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

    // Full verification: script formats, timelock, P2TR output, amounts, array consistency
    super::taproot_verification::verify_taproot_contract_data(
        &data,
        state.timelock,
        maker.network_port(),
    )?;
    #[cfg(debug_assertions)]
    log::debug!(
        "[CONTRACT_STATE] Role: Maker | Protocol: Taproot | SwapID: {} | ContractTxs: {} | Amounts: {} | Timelock: {} | Status: verified",
        data.id,
        data.contract_txs.len(),
        data.amounts.len(),
        state.timelock
    );

    let n = data.contract_txs.len();
    let mut incoming_swapcoins = Vec::with_capacity(n);
    for j in 0..n {
        let incoming_contract_tx = data.contract_txs[j].clone();
        maker.verify_contract_tx_on_chain(&incoming_contract_tx.compute_txid())?;

        let incoming_funding_amount = data.amounts[j];

        let other_pubkey = data.pubkeys.get(j).cloned().unwrap_or(data.next_hop_point);

        let mut incoming_swapcoin = IncomingSwapCoin::new_taproot(
            hashlock_privkey,
            data.hashlock_script.clone(),
            data.timelock_scripts[j].clone(),
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
        incoming_swapcoin.internal_key = Some(data.internal_keys[j]);
        incoming_swapcoin.tap_tweak = Some(data.tap_tweak_scalar(j)?);
        incoming_swapcoins.push(incoming_swapcoin);
    }

    let total_incoming = data.amounts.iter().cloned().sum();
    let swap_fee = maker.calculate_swap_fee(total_incoming, state.refund_locktime_offset as u32);
    let mining_fee = Amount::from_sat(estimate_funding_tx_fee_sats() * n as u64);
    let fee = swap_fee + mining_fee;
    let outgoing_total = total_incoming
        .checked_sub(fee)
        .ok_or(MakerError::General("Fee exceeds incoming amount"))?;
    let total_sat = total_incoming.to_sat() as u128;
    let mut outgoing_amounts = data
        .amounts
        .iter()
        .map(|amt| {
            let share = (fee.to_sat() as u128 * amt.to_sat() as u128 / total_sat) as u64;
            Amount::from_sat(amt.to_sat() - share)
        })
        .collect::<Vec<Amount>>();
    // Flooring leaves the outgoing sum a few sats above total fee; deduct that remainder from the largest output so the totals stay exact and deterministic.
    let remainder =
        outgoing_amounts.iter().map(|amt| amt.to_sat()).sum::<u64>() - outgoing_total.to_sat();
    if remainder > 0 {
        let max_idx = outgoing_amounts
            .iter()
            .enumerate()
            .max_by_key(|(_, amt)| amt.to_sat())
            .map(|(idx, _)| idx)
            .unwrap_or(0);
        outgoing_amounts[max_idx] -= Amount::from_sat(remainder);
    }

    log::info!(
        "[{}] Fee calculation: incoming_total={}, swap_fee={}, mining_fee={}, outgoing_total={}",
        maker.network_port(),
        total_incoming,
        swap_fee,
        mining_fee,
        outgoing_total
    );

    let hash = extract_hash_from_hashlock(&data.hashlock_script)
        .map_err(|e| MakerError::General(format!("Invalid hashlock script: {:?}", e).leak()))?;
    // If next_hashlock_nonce is provided, tweak the next hop's pubkey.
    // Otherwise (last maker → taker), use the un-tweaked next_hop_point.
    // The hashlock script (next hop's hashlock key) is shared across all contracts.
    let next_hop_hashlock_pubkey = if let Some(ref nonce) = data.next_hashlock_nonce {
        calculate_pubkey_from_nonce(&data.next_hop_point, nonce)
            .map_err(|e| MakerError::General(format!("Next hop key derivation: {:?}", e).leak()))?
    } else {
        data.next_hop_point
    };
    let next_hop_xonly = bitcoin::key::XOnlyPublicKey::from(next_hop_hashlock_pubkey.inner);
    let hashlock_script = create_hashlock_script(&hash, &next_hop_xonly);

    // Build one outgoing contract (with its own keys/scripts/address) per contract.
    let mut outgoing_swapcoins = Vec::with_capacity(n);
    let mut response_pubkeys = Vec::with_capacity(n);
    let mut response_internal_keys = Vec::with_capacity(n);
    let mut response_tap_tweaks = Vec::with_capacity(n);
    let mut response_timelock_scripts = Vec::with_capacity(n);
    let mut response_contract_txs = Vec::with_capacity(n);
    let mut response_amounts = Vec::with_capacity(n);
    let mut reserved = Vec::with_capacity(n);
    let mut spent_inputs: Vec<OutPoint> = Vec::new();

    let base_excluded = maker.collect_excluded_utxos(&data.id);

    for &outgoing_amount in &outgoing_amounts {
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
        let timelock_keypair =
            bitcoin::secp256k1::Keypair::from_secret_key(&secp, &timelock_privkey);
        let timelock_xonly = bitcoin::secp256k1::XOnlyPublicKey::from_keypair(&timelock_keypair).0;
        let timelock_script = {
            // For Taproot, state.timelock is already an absolute block height.
            let locktime = bitcoin::absolute::LockTime::from_height(state.timelock)
                .map_err(|_| MakerError::General("Invalid locktime height"))?;
            create_timelock_script(locktime, &timelock_xonly)
        };

        let builder = bitcoin::taproot::TaprootBuilder::new()
            .add_leaf(1, hashlock_script.clone())
            .map_err(|e| {
                MakerError::General(format!("Failed to add hashlock leaf: {:?}", e).leak())
            })?
            .add_leaf(1, timelock_script.clone())
            .map_err(|e| {
                MakerError::General(format!("Failed to add timelock leaf: {:?}", e).leak())
            })?;

        let mut ordered_pubkeys = [outgoing_pubkey, data.next_hop_point];
        ordered_pubkeys.sort_by_key(|a| a.inner.serialize());
        let internal_key = crate::protocol::musig_interface::get_aggregated_pubkey_compat(
            ordered_pubkeys[0].inner,
            ordered_pubkeys[1].inner,
        )
        .map_err(|e| {
            MakerError::General(format!("Failed to create aggregated pubkey: {:?}", e).leak())
        })?;

        let tap_info = builder.finalize(&secp, internal_key).map_err(|e| {
            MakerError::General(format!("Failed to finalize taproot: {:?}", e).leak())
        })?;
        let taproot_address =
            bitcoin::Address::p2tr_tweaked(tap_info.output_key(), maker.network());

        // Exclude UTXOs from other swaps and the inputs already spent this hop.
        let mut excluded_utxos = base_excluded.clone();
        excluded_utxos.extend(spent_inputs.iter().cloned());

        // QA: Integration-only malicious maker path for taker validation tests.
        #[cfg(feature = "integration-test")]
        let funding_amount = {
            use super::handlers::MakerBehavior;
            if maker.behavior() == MakerBehavior::UnderfundTaprootContract {
                Amount::from_sat(10_000)
            } else {
                outgoing_amount
            }
        };
        #[cfg(not(feature = "integration-test"))]
        let funding_amount = outgoing_amount;

        let (contract_tx, output_pos) = maker.create_funding_transaction(
            funding_amount,
            taproot_address.clone(),
            Some(excluded_utxos),
        )?;
        spent_inputs.extend(contract_tx.input.iter().map(|i| i.previous_output));
        let contract_outpoint = OutPoint {
            txid: contract_tx.compute_txid(),
            vout: output_pos,
        };
        reserved.push(contract_outpoint);
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

        // QA: Keep this claim inconsistent with the funded output when the test
        // behavior is active, proving the taker binds metadata to transaction value.
        #[cfg(feature = "integration-test")]
        let response_amount = {
            use super::handlers::MakerBehavior;
            if maker.behavior() == MakerBehavior::UnderfundTaprootContract {
                outgoing_amount
            } else {
                contract_output_amount
            }
        };
        #[cfg(not(feature = "integration-test"))]
        let response_amount = contract_output_amount;

        response_pubkeys.push(outgoing_pubkey);
        response_internal_keys.push(internal_key);
        response_tap_tweaks.push(SerializableScalar::from_bytes(
            tap_info.tap_tweak().to_scalar().to_be_bytes().to_vec(),
        ));
        response_timelock_scripts.push(timelock_script);
        response_contract_txs.push(contract_tx);
        response_amounts.push(response_amount);
        outgoing_swapcoins.push(outgoing_swapcoin);
    }

    state.reserve_utxo = reserved.clone();
    state.incoming_swapcoins = incoming_swapcoins.clone();
    state.outgoing_swapcoins = outgoing_swapcoins.clone();
    #[cfg(debug_assertions)]
    log::debug!(
        "[CONTRACT_STATE] Role: Maker | Protocol: Taproot | SwapID: {} | Contracts: {} | IncomingTotal: {} | ReservedUtxos: {}",
        data.id,
        n,
        total_incoming.to_sat(),
        state.reserve_utxo.len()
    );

    #[cfg(feature = "integration-test")]
    {
        use super::handlers::MakerBehavior;
        if maker.behavior() == MakerBehavior::SkipFundingBroadcast {
            log::warn!(
                "[{}] Test behavior: skipping Taproot funding broadcast",
                maker.network_port()
            );
            state.funding_broadcast = true;
            state.phase = SwapPhase::AwaitingPrivateKeyHandover;
            for incoming in &incoming_swapcoins {
                maker.save_incoming_swapcoin(incoming)?;
            }
            for outgoing in &outgoing_swapcoins {
                maker.save_outgoing_swapcoin(outgoing)?;
            }
            maker.store_connection_state(&data.id, state)?;
            return Err(MakerError::General("Test: skipped funding broadcast"));
        }
    }

    // Persist swapcoins before broadcasting contract txs. A later broadcast
    // failure can leave earlier Taproot contract txs on-chain, and the wallet
    // needs these records for timelock recovery after a restart.
    for incoming in &incoming_swapcoins {
        maker.save_incoming_swapcoin(incoming)?;
    }
    for outgoing in &outgoing_swapcoins {
        maker.save_outgoing_swapcoin(outgoing)?;
    }

    for (outgoing, contract_outpoint) in outgoing_swapcoins.iter().zip(reserved.iter()) {
        match maker.broadcast_transaction(&outgoing.contract_tx) {
            Ok(txid) => {
                log::info!(
                    "[{}] Broadcast Taproot contract tx {} for swap {}",
                    maker.network_port(),
                    txid,
                    data.id
                );
            }
            Err(MakerError::Wallet(WalletError::Rpc(BitcoinRpcError::JsonRpc(
                JsonRpcError::Rpc(rpc_error),
            )))) if rpc_error.code == -27 || {
                let message = rpc_error.message.to_ascii_lowercase();
                message.contains("already in block chain")
                    || message.contains("already in mempool")
                    || message.contains("already in utxo set")
                    || message.contains("txn-already-in-mempool")
            } =>
            {
                let txid = outgoing.contract_tx.compute_txid();
                log::info!(
                    "[{}] Taproot contract tx {} for swap {} was already broadcast",
                    maker.network_port(),
                    txid,
                    data.id
                );
            }
            Err(e) => return Err(e),
        }
        maker.register_watch_outpoint(
            *contract_outpoint,
            outgoing.contract_tx.output[contract_outpoint.vout as usize]
                .script_pubkey
                .clone(),
        );
    }

    state.funding_broadcast = true;
    state.phase = SwapPhase::AwaitingPrivateKeyHandover;

    maker.store_connection_state(&data.id, state)?;

    #[cfg(feature = "integration-test")]
    {
        use super::handlers::MakerBehavior;
        if maker.behavior() == MakerBehavior::BroadcastContractAfterSetup {
            log::warn!(
                "[{}] Test behavior: broadcasting contract tx after taproot setup, then closing",
                maker.network_port()
            );
            if let Some(outgoing) = outgoing_swapcoins.first() {
                let _ = maker.broadcast_transaction(&outgoing.contract_tx);
            }
            return Err(MakerError::General("Test: broadcast contract after setup"));
        }
    }

    log::info!(
        "[{}] Created {} Taproot swapcoins for swap {}",
        maker.network_port(),
        n,
        data.id
    );

    let response = TaprootContractData::new(
        data.id.clone(),
        response_pubkeys,
        tweakable_pubkey,
        response_internal_keys,
        response_tap_tweaks,
        hashlock_script,
        response_timelock_scripts,
        response_contract_txs,
        response_amounts,
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
#[hotpath::measure]
fn process_taproot_handover<M: Maker>(
    maker: &Arc<M>,
    state: &mut ConnectionState,
    handover: PrivateKeyHandover,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    state.expect_phase(&[SwapPhase::AwaitingPrivateKeyHandover])?;
    state.check_swap_id(&handover.id)?;

    #[cfg(feature = "integration-test")]
    {
        use super::handlers::MakerBehavior;
        if maker.behavior() == MakerBehavior::CloseAtPrivateKeyHandover {
            log::warn!(
                "[{}] Test behavior: closing at taproot private key handover",
                maker.network_port()
            );
            return Err(MakerError::General("Test: closing at private key handover"));
        }
    }

    log::info!(
        "[{}] Processing Taproot private key handover for swap {}",
        maker.network_port(),
        handover.id
    );

    // Verify the received private keys before proceeding
    super::taproot_verification::verify_taproot_privkey_handover(
        &handover.privkeys,
        &state.incoming_swapcoins,
        maker.network_port(),
    )?;

    if state.outgoing_swapcoins.is_empty() {
        return Err(MakerError::General("No outgoing swapcoin found"));
    }
    let mut privkeys = Vec::with_capacity(state.outgoing_swapcoins.len());
    for outgoing in &state.outgoing_swapcoins {
        privkeys.push(SwapPrivkey {
            identifier: bitcoin::ScriptBuf::new(),
            key: outgoing
                .my_privkey
                .ok_or(MakerError::General("No private key in outgoing swapcoin"))?,
        });
    }

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
    #[cfg(debug_assertions)]
    log::debug!(
        "[SWAP_STATE] Source: maker::taproot_handlers::process_taproot_handover | SwapID: {} | Protocol: Taproot | Phase: Completed | Incoming: {} | Outgoing: {}",
        handover.id,
        state.incoming_swapcoins.len(),
        state.outgoing_swapcoins.len()
    );

    // Generate and save maker success report
    emit_maker_success_report(maker, state, &handover.id);

    #[cfg(feature = "integration-test")]
    {
        use super::handlers::MakerBehavior;
        if maker.behavior() == MakerBehavior::CloseAfterSweep {
            log::warn!(
                "[{}] Test behavior: closing after sweep / completing handover",
                maker.network_port()
            );
            return Err(MakerError::General("Test: closing after sweep"));
        }
    }

    log::info!(
        "[{}] Taproot swap {} completed successfully, returning private key",
        maker.network_port(),
        handover.id
    );

    let response = PrivateKeyHandover {
        id: handover.id,
        privkeys,
    };

    Ok(Some(MakerToTakerMessage::TaprootPrivateKeyHandover(
        response,
    )))
}

/// Emit a maker success report after private key handover.
#[hotpath::measure]
fn emit_maker_success_report<M: Maker>(maker: &Arc<M>, state: &ConnectionState, swap_id: &str) {
    let incoming_total: u64 = state
        .incoming_swapcoins
        .iter()
        .map(|s| s.funding_amount.to_sat())
        .sum();
    let outgoing_total: u64 = state
        .outgoing_swapcoins
        .iter()
        .map(|s| s.funding_amount.to_sat())
        .sum();
    let incoming_txid = state
        .incoming_swapcoins
        .first()
        .map(|s| s.contract_tx.compute_txid().to_string())
        .unwrap_or_else(|| "N/A".to_string());
    let outgoing_txid = state
        .outgoing_swapcoins
        .first()
        .map(|s| s.contract_tx.compute_txid().to_string())
        .unwrap_or_else(|| "N/A".to_string());
    let timelock = state
        .outgoing_swapcoins
        .first()
        .and_then(|s| s.get_timelock())
        .unwrap_or(0);
    let network = maker.network().to_string();

    let report = MakerReport::success(
        swap_id.to_string(),
        state.swap_start_time,
        incoming_total,
        outgoing_total,
        state.service_fee_sats,
        incoming_txid,
        outgoing_txid,
        timelock,
        network,
        state.incoming_swapcoins.first(),
        state.outgoing_swapcoins.first(),
    );
    report.print();
    if let Err(e) = report.save_for_wallet(maker.data_dir(), Some(maker.wallet_name())) {
        log::warn!("Failed to save maker success report: {:?}", e);
    }
}

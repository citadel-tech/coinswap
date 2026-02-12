//! Taproot (MuSig2) specific swap methods for the Unified Taker.

use bitcoin::{
    hashes::Hash,
    secp256k1::{self, rand::rngs::OsRng, Secp256k1, SecretKey},
    Amount, Network, OutPoint, PublicKey, ScriptBuf,
};

use crate::{
    protocol::{
        contract2::{create_hashlock_script, create_timelock_script},
        router::{MakerToTakerMessage, TakerToMakerMessage},
        taproot_messages::{SerializableScalar, TaprootContractData},
    },
    utill::{read_message, send_message, MIN_FEE_RATE},
    wallet::{
        unified_swapcoin::{IncomingSwapCoin, OutgoingSwapCoin, WatchOnlySwapCoin},
        Wallet,
    },
};

use super::{error::TakerError, unified_api::UnifiedTaker};

/// Build contract data from a previous maker's response (for forwarding to the next maker).
#[allow(clippy::type_complexity)]
fn build_contract_data_from_response(
    prev: &TaprootContractData,
) -> (
    Vec<PublicKey>,
    Vec<ScriptBuf>,
    Vec<ScriptBuf>,
    secp256k1::XOnlyPublicKey,
    SerializableScalar,
    Vec<bitcoin::Transaction>,
    Vec<Amount>,
) {
    (
        prev.pubkeys.clone(),
        vec![prev.hashlock_script.clone()],
        vec![prev.timelock_script.clone()],
        prev.internal_key,
        prev.tap_tweak.clone(),
        prev.contract_txs.clone(),
        prev.amounts.clone(),
    )
}

impl UnifiedTaker {
    /// Create Taproot (MuSig2) contract transactions and swapcoins (static version).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn create_taproot_contracts_static(
        wallet: &mut Wallet,
        multisig_pubkeys: &[PublicKey],
        hashlock_pubkeys: &[PublicKey],
        preimage: [u8; 32],
        locktime: u16,
        send_amount: Amount,
        swap_id: &str,
        network: Network,
        manually_selected_outpoints: Option<Vec<OutPoint>>,
    ) -> Result<Vec<OutgoingSwapCoin>, TakerError> {
        let secp = Secp256k1::new();
        let mut swapcoins = Vec::new();

        for (multisig_pubkey, hashlock_pubkey) in
            multisig_pubkeys.iter().zip(hashlock_pubkeys.iter())
        {
            // Generate our keypair for this swap
            let my_privkey = SecretKey::new(&mut OsRng);
            let my_pubkey = PublicKey {
                compressed: true,
                inner: secp256k1::PublicKey::from_secret_key(&secp, &my_privkey),
            };

            // Convert to x-only pubkeys for Taproot
            let keypair = secp256k1::Keypair::from_secret_key(&secp, &my_privkey);
            let my_xonly = secp256k1::XOnlyPublicKey::from_keypair(&keypair).0;
            let (other_xonly, _parity) = hashlock_pubkey.inner.x_only_public_key();

            // Create hashlock and timelock scripts
            // For Taproot, use SHA256 hash of the preimage
            let sha256_hash: [u8; 32] =
                bitcoin::hashes::sha256::Hash::hash(&preimage).to_byte_array();
            let hashlock_script = create_hashlock_script(&sha256_hash, &other_xonly);
            let locktime_abs = bitcoin::absolute::LockTime::from_height(locktime as u32)
                .unwrap_or(bitcoin::absolute::LockTime::ZERO);
            let timelock_script = create_timelock_script(locktime_abs, &my_xonly);

            let builder = bitcoin::taproot::TaprootBuilder::new()
                .add_leaf(1, hashlock_script.clone())
                .map_err(|e| TakerError::General(format!("Failed to add hashlock leaf: {:?}", e)))?
                .add_leaf(1, timelock_script.clone())
                .map_err(|e| {
                    TakerError::General(format!("Failed to add timelock leaf: {:?}", e))
                })?;

            // Create aggregated MuSig2 pubkey for internal key (allows cooperative key-path spend)
            // Order pubkeys lexicographically to match signing order
            let mut ordered_pubkeys = [my_pubkey, *multisig_pubkey];
            ordered_pubkeys.sort_by(|a, b| a.inner.serialize().cmp(&b.inner.serialize()));
            let internal_key = crate::protocol::musig_interface::get_aggregated_pubkey_compat(
                ordered_pubkeys[0].inner,
                ordered_pubkeys[1].inner,
            )
            .map_err(|e| {
                TakerError::General(format!("Failed to create aggregated pubkey: {:?}", e))
            })?;

            let tap_info = builder
                .finalize(&secp, internal_key)
                .map_err(|e| TakerError::General(format!("Failed to finalize taproot: {:?}", e)))?;

            // Create Taproot address
            let taproot_address = bitcoin::Address::p2tr_tweaked(tap_info.output_key(), network);

            let funding_result = wallet.create_funding_txes(
                send_amount,
                std::slice::from_ref(&taproot_address),
                MIN_FEE_RATE,
                manually_selected_outpoints.clone(),
            )?;

            for (contract_tx, &output_pos) in funding_result
                .funding_txes
                .iter()
                .zip(funding_result.payment_output_positions.iter())
            {
                let contract_amount = contract_tx.output[output_pos as usize].value;

                // Create outgoing swapcoin with Taproot data.
                let mut outgoing = OutgoingSwapCoin::new_taproot(
                    my_privkey,
                    hashlock_script.clone(),
                    timelock_script.clone(),
                    contract_tx.clone(),
                    contract_amount,
                );
                outgoing.swap_id = Some(swap_id.to_string());
                outgoing.set_taproot_params(
                    my_privkey,
                    my_pubkey,
                    *multisig_pubkey,
                    internal_key,
                    tap_info.tap_tweak().to_scalar(),
                );

                swapcoins.push(outgoing);
            }
        }

        Ok(swapcoins)
    }

    /// Exchange contract data with makers (Taproot protocol).
    pub(crate) fn exchange_contract_data(&mut self) -> Result<(), TakerError> {
        log::info!("Exchanging contract data with makers...");

        let num_makers = self.swap_state()?.makers.len();
        let mut received_contracts: Vec<TaprootContractData> = Vec::new();

        for i in 0..num_makers {
            let maker_address = self.swap_state()?.makers[i]
                .offer_and_address
                .address
                .to_string();
            let mut stream = self.connect_to_maker(&maker_address)?;

            self.handshake_maker(&mut stream)?;

            let (
                pubkeys,
                hashlock_scripts,
                timelock_scripts,
                internal_key,
                tap_tweak,
                contract_txs,
                amounts,
            ) = if i == 0 {
                self.build_contract_data_from_outgoing()?
            } else {
                build_contract_data_from_response(&received_contracts[i - 1])
            };

            let secp = Secp256k1::new();
            let my_privkey = SecretKey::new(&mut OsRng);
            let my_pubkey = PublicKey {
                compressed: true,
                inner: secp256k1::PublicKey::from_secret_key(&secp, &my_privkey),
            };

            let next_hop_point = if i + 1 < self.swap_state()?.makers.len() {
                self.swap_state()?.makers[i + 1]
                    .tweakable_point
                    .unwrap_or(my_pubkey)
            } else {
                my_pubkey
            };

            log::info!(
                "Sending contract data to maker {}: {} pubkeys, {} contract_txs",
                i,
                pubkeys.len(),
                contract_txs.len()
            );

            let contract_data = TaprootContractData::new(
                self.swap_state()?.id.clone(),
                pubkeys,
                next_hop_point,
                internal_key,
                tap_tweak,
                hashlock_scripts.first().cloned().unwrap_or_default(),
                timelock_scripts.first().cloned().unwrap_or_default(),
                contract_txs,
                amounts,
            );

            send_message(
                &mut stream,
                &TakerToMakerMessage::TaprootContractData(Box::new(contract_data)),
            )?;

            let msg_bytes = read_message(&mut stream)?;
            let msg: MakerToTakerMessage = serde_cbor::from_slice(&msg_bytes)?;

            match msg {
                MakerToTakerMessage::TaprootContractData(maker_contract) => {
                    log::info!(
                        "Received Taproot contract data from maker {}: {} contract_txs",
                        i,
                        maker_contract.contract_txs.len()
                    );

                    let is_last_maker = i == num_makers - 1;
                    if is_last_maker {
                        // Only the last maker's contract is addressed to the taker.
                        self.create_swapcoins_from_taproot_contract(&maker_contract, my_privkey)?;
                    } else {
                        // Intermediate contracts (maker→maker) are watch-only for the taker.
                        let sender_pubkey =
                            maker_contract.pubkeys.first().cloned().unwrap_or(my_pubkey);
                        let contract_tx =
                            maker_contract
                                .contract_txs
                                .first()
                                .cloned()
                                .ok_or_else(|| {
                                    TakerError::General(
                                        "No contract tx in maker response".to_string(),
                                    )
                                })?;
                        let funding_amount = maker_contract
                            .amounts
                            .first()
                            .cloned()
                            .unwrap_or(Amount::ZERO);

                        let watchonly = WatchOnlySwapCoin::new_taproot(
                            sender_pubkey,
                            maker_contract.next_hop_point,
                            contract_tx,
                            maker_contract.hashlock_script.clone(),
                            maker_contract.timelock_script.clone(),
                            funding_amount,
                        );

                        let swap_id = self.swap_state()?.id.clone();
                        {
                            let mut wallet = self.write_wallet()?;
                            wallet
                                .add_unified_watchonly_swapcoins(&swap_id, vec![watchonly.clone()]);
                            wallet.save_to_disk()?;
                        }
                        self.swap_state_mut()?.watchonly_swapcoins.push(watchonly);
                    }

                    received_contracts.push(*maker_contract);
                }
                _ => {
                    return Err(TakerError::General(format!(
                        "Unexpected message from maker {}: expected TaprootContractData",
                        i
                    )));
                }
            }
        }

        Ok(())
    }

    /// Build contract data from our outgoing swapcoins (first hop).
    #[allow(clippy::type_complexity)]
    fn build_contract_data_from_outgoing(
        &self,
    ) -> Result<
        (
            Vec<PublicKey>,
            Vec<ScriptBuf>,
            Vec<ScriptBuf>,
            secp256k1::XOnlyPublicKey,
            SerializableScalar,
            Vec<bitcoin::Transaction>,
            Vec<Amount>,
        ),
        TakerError,
    > {
        let mut pubkeys = Vec::new();
        let mut hashlock_scripts = Vec::new();
        let mut timelock_scripts = Vec::new();
        let mut contract_txs = Vec::new();
        let mut amounts = Vec::new();

        for swapcoin in &self.swap_state()?.outgoing_swapcoins {
            if let Some(pubkey) = swapcoin.my_pubkey {
                pubkeys.push(pubkey);
            }

            if let Some(hl_script) = swapcoin.hashlock_script() {
                hashlock_scripts.push(hl_script.clone());
            }

            if let Some(tl_script) = swapcoin.timelock_script() {
                timelock_scripts.push(tl_script.clone());
            }

            contract_txs.push(swapcoin.contract_tx.clone());
            amounts.push(swapcoin.funding_amount);
        }

        let first_swapcoin = self
            .swap_state()?
            .outgoing_swapcoins
            .first()
            .ok_or_else(|| TakerError::General("No outgoing swapcoins".to_string()))?;

        let internal_key = first_swapcoin.internal_key.ok_or_else(|| {
            TakerError::General("Outgoing swapcoin missing internal_key".to_string())
        })?;
        let tweak_bytes = first_swapcoin
            .tap_tweak
            .map(|s| s.to_be_bytes())
            .unwrap_or([0u8; 32]);
        let tap_tweak = SerializableScalar::from_bytes(tweak_bytes.to_vec());

        Ok((
            pubkeys,
            hashlock_scripts,
            timelock_scripts,
            internal_key,
            tap_tweak,
            contract_txs,
            amounts,
        ))
    }

    /// Create swapcoins from received Taproot contract data.
    fn create_swapcoins_from_taproot_contract(
        &mut self,
        contract: &TaprootContractData,
        my_privkey: SecretKey,
    ) -> Result<(), TakerError> {
        let secp = Secp256k1::new();

        let contract_tx =
            contract.contract_txs.first().cloned().ok_or_else(|| {
                TakerError::General("No contract tx in contract data".to_string())
            })?;

        let amount =
            contract.amounts.first().cloned().ok_or_else(|| {
                TakerError::General("No amount in Taproot contract data".to_string())
            })?;

        let hashlock_privkey = SecretKey::new(&mut OsRng);

        let mut swapcoin = IncomingSwapCoin::new_taproot(
            hashlock_privkey,
            contract.hashlock_script.clone(),
            contract.timelock_script.clone(),
            contract_tx,
            amount,
        );

        let other_pubkey =
            contract.pubkeys.first().cloned().ok_or_else(|| {
                TakerError::General("No pubkey in Taproot contract data".to_string())
            })?;

        swapcoin.my_privkey = Some(my_privkey);
        swapcoin.my_pubkey = Some(PublicKey {
            compressed: true,
            inner: secp256k1::PublicKey::from_secret_key(&secp, &my_privkey),
        });
        swapcoin.other_pubkey = Some(other_pubkey);
        swapcoin.internal_key = Some(contract.internal_key);
        swapcoin.tap_tweak = Some(contract.tap_tweak.clone().into());

        swapcoin.swap_id = Some(contract.id.clone());
        self.swap_state_mut()?.incoming_swapcoins.push(swapcoin);
        Ok(())
    }

    /// Broadcast contract transactions (Taproot).
    pub(crate) fn broadcast_contract_txs(&mut self) -> Result<(), TakerError> {
        log::info!("Broadcasting contract transactions...");

        let wallet = self.write_wallet()?;

        for swapcoin in &self.swap_state()?.outgoing_swapcoins {
            let txid = wallet.send_tx(&swapcoin.contract_tx).map_err(|e| {
                TakerError::General(format!("Failed to broadcast contract tx: {:?}", e))
            })?;

            log::info!("Broadcast contract tx: {}", txid);

            let vout = swapcoin
                .contract_tx
                .output
                .iter()
                .position(|o| o.value == swapcoin.funding_amount)
                .unwrap_or(0) as u32;
            let outpoint = OutPoint { txid, vout };
            self.watch_service.register_watch_request(outpoint);
        }

        wallet.save_to_disk()?;
        drop(wallet);

        let contract_txids: Vec<_> = self
            .swap_state()?
            .outgoing_swapcoins
            .iter()
            .map(|sc| sc.contract_tx.compute_txid())
            .collect();
        self.wait_for_txids_confirmation(&contract_txids)?;

        log::info!("Contract transactions broadcast and confirmed");
        Ok(())
    }
}

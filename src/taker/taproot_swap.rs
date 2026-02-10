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
        unified_swapcoin::{IncomingSwapCoin, OutgoingSwapCoin},
        Wallet,
    },
};

use super::{error::TakerError, unified_api::UnifiedTaker};

impl UnifiedTaker {
    /// Create Taproot (MuSig2) funding transactions and swapcoins (static version).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn create_taproot_funding_static(
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
            let hashlock_script = create_hashlock_script(&sha256_hash, &my_xonly);
            let locktime_abs = bitcoin::absolute::LockTime::from_height(locktime as u32)
                .unwrap_or(bitcoin::absolute::LockTime::ZERO);
            let timelock_script = create_timelock_script(locktime_abs, &other_xonly);

            // Create Taproot output using TaprootBuilder
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

            for (funding_tx, &output_pos) in funding_result
                .funding_txes
                .iter()
                .zip(funding_result.payment_output_positions.iter())
            {
                let funding_amount = funding_tx.output[output_pos as usize].value;

                // For Taproot, the funding tx IS the contract — it directly creates
                // the P2TR output with hashlock/timelock script paths. There's no
                // separate contract transaction like in Legacy.
                let contract_tx = funding_tx.clone();

                // Create outgoing swapcoin with Taproot data
                let mut outgoing = OutgoingSwapCoin::new_taproot(
                    my_privkey,
                    hashlock_script.clone(),
                    timelock_script.clone(),
                    contract_tx,
                    funding_amount,
                );
                outgoing.swap_id = Some(swap_id.to_string());
                outgoing.funding_tx = Some(funding_tx.clone());
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

        let secp = Secp256k1::new();

        for i in 0..self.ongoing_swap.makers.len() {
            let maker_address = self.ongoing_swap.makers[i]
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
            ) = if i == 0 && !self.ongoing_swap.outgoing_swapcoins.is_empty() {
                let (pks, hl_scripts, tl_scripts, ik, tw) =
                    self.build_contract_data_from_outgoing()?;

                let mut contract_txs = Vec::new();
                let mut amounts = Vec::new();

                for swapcoin in &self.ongoing_swap.outgoing_swapcoins {
                    if let Some(funding_tx) = &swapcoin.funding_tx {
                        contract_txs.push(funding_tx.clone());
                    }
                    amounts.push(swapcoin.funding_amount);
                }

                (pks, hl_scripts, tl_scripts, ik, tw, contract_txs, amounts)
            } else {
                let prev_incoming =
                    self.ongoing_swap
                        .incoming_swapcoins
                        .get(i - 1)
                        .ok_or_else(|| {
                            TakerError::General(format!(
                                "Missing incoming swapcoin from previous hop {}",
                                i - 1
                            ))
                        })?;

                let contract_txs = vec![prev_incoming.contract_tx.clone()];
                let amounts = vec![prev_incoming.funding_amount];

                let pubkeys = vec![prev_incoming.other_pubkey.ok_or_else(|| {
                    TakerError::General(
                        "Previous incoming swapcoin missing other_pubkey".to_string(),
                    )
                })?];

                let internal_key = prev_incoming.internal_key.ok_or_else(|| {
                    TakerError::General(
                        "Previous incoming swapcoin missing internal_key".to_string(),
                    )
                })?;
                let tweak_bytes = prev_incoming
                    .tap_tweak
                    .map(|s| s.to_be_bytes())
                    .unwrap_or([0u8; 32]);
                let tap_tweak = SerializableScalar::from_bytes(tweak_bytes.to_vec());

                let hashlock_scripts = prev_incoming
                    .hashlock_script
                    .as_ref()
                    .map(|s| vec![s.clone()])
                    .unwrap_or_default();
                let timelock_scripts = prev_incoming
                    .timelock_script
                    .as_ref()
                    .map(|s| vec![s.clone()])
                    .unwrap_or_default();

                (
                    pubkeys,
                    hashlock_scripts,
                    timelock_scripts,
                    internal_key,
                    tap_tweak,
                    contract_txs,
                    amounts,
                )
            };

            let my_privkey = SecretKey::new(&mut OsRng);
            let my_pubkey = PublicKey {
                compressed: true,
                inner: secp256k1::PublicKey::from_secret_key(&secp, &my_privkey),
            };

            let next_hop_point = if i + 1 < self.ongoing_swap.makers.len() {
                self.ongoing_swap.makers[i + 1]
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
                self.ongoing_swap.id.clone(),
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
                    self.create_swapcoins_from_taproot_contract(&maker_contract, my_privkey)?;
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

    /// Build contract data message from our outgoing swapcoins (Taproot).
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
        ),
        TakerError,
    > {
        let mut pubkeys = Vec::new();
        let mut hashlock_scripts = Vec::new();
        let mut timelock_scripts = Vec::new();

        for swapcoin in &self.ongoing_swap.outgoing_swapcoins {
            if let Some(pubkey) = swapcoin.my_pubkey {
                pubkeys.push(pubkey);
            }

            if let Some(hl_script) = swapcoin.hashlock_script() {
                hashlock_scripts.push(hl_script.clone());
            }

            if let Some(tl_script) = swapcoin.timelock_script() {
                timelock_scripts.push(tl_script.clone());
            }
        }

        let first_swapcoin = self
            .ongoing_swap
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

        let amount = contract
            .amounts
            .first()
            .cloned()
            .unwrap_or(self.ongoing_swap.params.send_amount);

        let hashlock_privkey = SecretKey::new(&mut OsRng);

        let mut swapcoin = IncomingSwapCoin::new_taproot(
            hashlock_privkey,
            contract.hashlock_script.clone(),
            contract.timelock_script.clone(),
            contract_tx,
            amount,
        );

        let other_pubkey = contract
            .pubkeys
            .first()
            .cloned()
            .unwrap_or_else(|| PublicKey {
                compressed: true,
                inner: secp256k1::PublicKey::from_secret_key(&secp, &my_privkey),
            });

        swapcoin.my_privkey = Some(my_privkey);
        swapcoin.my_pubkey = Some(PublicKey {
            compressed: true,
            inner: secp256k1::PublicKey::from_secret_key(&secp, &my_privkey),
        });
        swapcoin.other_pubkey = Some(other_pubkey);
        swapcoin.internal_key = Some(contract.internal_key);
        swapcoin.tap_tweak = Some(contract.tap_tweak.clone().into());

        swapcoin.swap_id = Some(contract.id.clone());
        self.ongoing_swap.incoming_swapcoins.push(swapcoin);
        Ok(())
    }

    /// Create and broadcast funding transactions (Taproot).
    pub(crate) fn create_and_broadcast_funding(&mut self) -> Result<(), TakerError> {
        log::info!("Creating and broadcasting funding transactions...");

        let wallet = self
            .wallet
            .write()
            .map_err(|_| TakerError::General("Failed to lock wallet".to_string()))?;

        for swapcoin in &self.ongoing_swap.outgoing_swapcoins {
            if let Some(funding_tx) = &swapcoin.funding_tx {
                match wallet.send_tx(funding_tx) {
                    Ok(txid) => {
                        log::info!("Broadcast funding tx: {}", txid);

                        let outpoint = OutPoint {
                            txid,
                            vout: swapcoin
                                .contract_tx
                                .input
                                .first()
                                .map(|i| i.previous_output.vout)
                                .unwrap_or(0),
                        };
                        self.watch_service.register_watch_request(outpoint);
                    }
                    Err(e) => {
                        log::error!("Failed to broadcast funding tx: {:?}", e);
                    }
                }
            } else {
                log::warn!(
                    "No funding tx found for swapcoin, swap_id: {:?}",
                    swapcoin.swap_id
                );
            }
        }

        wallet.save_to_disk()?;
        drop(wallet);

        let funding_txids: Vec<_> = self
            .ongoing_swap
            .outgoing_swapcoins
            .iter()
            .filter_map(|sc| sc.funding_tx.as_ref().map(|tx| tx.compute_txid()))
            .collect();
        self.wait_for_txids_confirmation(&funding_txids)?;

        log::info!("Funding transactions created and broadcast");
        Ok(())
    }
}

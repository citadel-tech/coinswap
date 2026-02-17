//! Legacy (ECDSA) specific swap methods for the Unified Taker.

use std::{net::TcpStream, time::Duration};

use bitcoin::{
    hashes::{hash160::Hash as Hash160, Hash},
    secp256k1::{self, rand::rngs::OsRng, Secp256k1, SecretKey},
    Amount, Network, OutPoint, PublicKey, ScriptBuf, Transaction, Txid,
};

use crate::{
    protocol::{
        contract::{
            create_contract_redeemscript, create_multisig_redeemscript, create_senders_contract_tx,
            read_pubkeys_from_multisig_redeemscript, sign_contract_tx,
        },
        legacy_messages::{
            ContractTxInfoForRecvr, ContractTxInfoForSender, FundingTxInfo, NextHopInfo,
            ProofOfFunding, ReqContractSigsForRecvr, ReqContractSigsForSender,
            RespContractSigsForRecvrAndSender, SenderContractTxInfo,
        },
        router::{MakerToTakerMessage, TakerToMakerMessage},
    },
    utill::{generate_keypair, generate_maker_keys, read_message, send_message, MIN_FEE_RATE},
    wallet::{
        unified_swapcoin::{IncomingSwapCoin, OutgoingSwapCoin, WatchOnlySwapCoin},
        Wallet,
    },
};

use super::{error::TakerError, unified_api::UnifiedTaker};

/// Delay to allow the maker to broadcast its funding transactions before we poll.
const MAKER_BROADCAST_DELAY: Duration = Duration::from_secs(2);

impl UnifiedTaker {
    /// Create Legacy (ECDSA) funding transactions and swapcoins (static version).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn funding_create_legacy(
        wallet: &mut Wallet,
        multisig_pubkeys: &[PublicKey],
        hashlock_pubkeys: &[PublicKey],
        hashvalue: Hash160,
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
            let my_multisig_privkey = SecretKey::new(&mut OsRng);
            let my_multisig_pubkey = PublicKey {
                compressed: true,
                inner: secp256k1::PublicKey::from_secret_key(&secp, &my_multisig_privkey),
            };

            let multisig_redeemscript =
                create_multisig_redeemscript(&my_multisig_pubkey, multisig_pubkey);

            let (timelock_pubkey, timelock_privkey) = generate_keypair();

            let contract_redeemscript = create_contract_redeemscript(
                hashlock_pubkey,
                &timelock_pubkey,
                &hashvalue,
                &locktime,
            );

            let coinswap_address = bitcoin::Address::p2wsh(&multisig_redeemscript, network);

            let funding_result = wallet.create_funding_txes(
                send_amount,
                &[coinswap_address],
                MIN_FEE_RATE,
                manually_selected_outpoints.clone(),
            )?;

            for (funding_tx, &output_pos) in funding_result
                .funding_txes
                .iter()
                .zip(funding_result.payment_output_positions.iter())
            {
                let funding_outpoint = OutPoint {
                    txid: funding_tx.compute_txid(),
                    vout: output_pos,
                };
                let funding_amount = funding_tx.output[output_pos as usize].value;

                let contract_tx = create_senders_contract_tx(
                    funding_outpoint,
                    funding_amount,
                    &contract_redeemscript,
                )?;

                let mut outgoing = OutgoingSwapCoin::new_legacy(
                    my_multisig_privkey,
                    *multisig_pubkey,
                    contract_tx,
                    contract_redeemscript.clone(),
                    timelock_privkey,
                    funding_amount,
                );
                outgoing.swap_id = Some(swap_id.to_string());
                outgoing.funding_tx = Some(funding_tx.clone());

                swapcoins.push(outgoing);
            }
        }

        Ok(swapcoins)
    }

    /// Execute the multi-hop Legacy coinswap flow.
    pub(crate) fn exchange_legacy(&mut self) -> Result<(), TakerError> {
        log::info!("Starting multi-hop Legacy swap with ProofOfFunding flow");

        let swap = self.swap_state()?;
        let swap_id = swap.id.clone();
        let maker_count = swap.makers.len();

        let mut prev_senders_info: Option<Vec<SenderContractTxInfo>> = None;
        // Taker's own keys for the last hop (set during last iteration)
        let mut taker_multisig_privkeys: Option<Vec<SecretKey>> = None;
        let mut taker_hashlock_privkeys: Option<Vec<SecretKey>> = None;

        // Background thread monitors funding outpoints for adversarial contract broadcasts.
        self.breach_detector =
            Some(super::background_services::BreachDetector::start(self.watch_service.clone()));

        for maker_idx in 0..maker_count {
            let maker_address = self.swap_state()?.makers[maker_idx]
                .offer_and_address
                .address
                .to_string();

            log::info!(
                "Processing maker {} of {}: {}",
                maker_idx + 1,
                maker_count,
                maker_address
            );

            // Connect to this maker
            let mut stream = self.net_connect(&maker_address)?;
            self.net_handshake(&mut stream)?;
            self.swap_state_mut()?.makers[maker_idx]
                .legacy_exchange_mut()?
                .connected = true;

            // Determine our position
            let is_first_peer = maker_idx == 0;
            let is_last_peer = maker_idx == maker_count - 1;

            let outgoing_locktime = super::unified_api::REFUND_LOCKTIME_BASE
                + super::unified_api::REFUND_LOCKTIME_STEP * (maker_count - maker_idx - 1) as u16;

            let (
                funding_txs,
                contract_redeemscripts,
                multisig_redeemscripts,
                multisig_nonces,
                hashlock_nonces,
            ) = if is_first_peer {
                let outgoing = &self.swap_state()?.outgoing_swapcoins;
                if outgoing.is_empty() {
                    return Err(TakerError::General(
                        "No outgoing swapcoins for first hop".to_string(),
                    ));
                }

                let funding_txs: Vec<Transaction> = outgoing
                    .iter()
                    .map(|sc| {
                        sc.funding_tx.clone().ok_or_else(|| {
                            TakerError::General("Outgoing swapcoin missing funding_tx".to_string())
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let contract_rs: Vec<ScriptBuf> = outgoing
                    .iter()
                    .map(|sc| {
                        sc.contract_redeemscript.clone().ok_or_else(|| {
                            TakerError::General(
                                "Outgoing swapcoin missing contract_redeemscript".to_string(),
                            )
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let multisig_rs: Vec<ScriptBuf> = outgoing
                    .iter()
                    .map(|sc| {
                        let my_pub = sc.my_pubkey.ok_or_else(|| {
                            TakerError::General("Outgoing swapcoin missing my_pubkey".to_string())
                        })?;
                        let other_pub = sc.other_pubkey.ok_or_else(|| {
                            TakerError::General(
                                "Outgoing swapcoin missing other_pubkey".to_string(),
                            )
                        })?;
                        Ok(create_multisig_redeemscript(&my_pub, &other_pub))
                    })
                    .collect::<Result<Vec<_>, TakerError>>()?;

                (
                    funding_txs,
                    contract_rs,
                    multisig_rs,
                    self.swap_state()?.multisig_nonces.clone(),
                    self.swap_state()?.hashlock_nonces.clone(),
                )
            } else {
                let prev_info = prev_senders_info.as_ref().ok_or_else(|| {
                    TakerError::General("No previous maker info for multi-hop".to_string())
                })?;

                let funding_txs: Vec<Transaction> = prev_info
                    .iter()
                    .map(|info| info.funding_tx.clone())
                    .collect();
                let contract_rs: Vec<ScriptBuf> = prev_info
                    .iter()
                    .map(|info| info.contract_redeemscript.clone())
                    .collect();
                let multisig_rs: Vec<ScriptBuf> = prev_info
                    .iter()
                    .map(|info| info.multisig_redeemscript.clone())
                    .collect();
                let multisig_nonces: Vec<SecretKey> =
                    prev_info.iter().map(|info| info.multisig_nonce).collect();
                let hashlock_nonces: Vec<SecretKey> =
                    prev_info.iter().map(|info| info.hashlock_nonce).collect();

                (
                    funding_txs,
                    contract_rs,
                    multisig_rs,
                    multisig_nonces,
                    hashlock_nonces,
                )
            };

            if is_first_peer {
                log::info!(
                    "Step 1: Requesting sender contract signatures from maker {}",
                    maker_idx
                );
                let sender_sigs = self.exchange_req_sender_sigs(
                    &mut stream,
                    &swap_id,
                    &self.swap_state()?.outgoing_swapcoins,
                    outgoing_locktime,
                )?;
                let swap = self.swap_state_mut()?;
                for (swapcoin, sig) in swap
                    .outgoing_swapcoins
                    .iter_mut()
                    .zip(sender_sigs.into_iter())
                {
                    swapcoin.others_contract_sig = Some(sig);
                }
                let exch = self.swap_state_mut()?.makers[maker_idx].legacy_exchange_mut()?;
                exch.sender_sigs_requested = true;
                exch.sender_sigs_received = true;
            } else {
                log::info!(
                    "Step 1: Skipping separate signature request (handled in ProofOfFunding flow)"
                );
            }

            if is_first_peer {
                log::info!(
                    "Step 1.5: Broadcasting funding transactions and waiting for confirmation"
                );
                {
                    let wallet = self.write_wallet()?;

                    for swapcoin in &self.swap_state()?.outgoing_swapcoins {
                        let funding_tx = swapcoin.funding_tx.as_ref().ok_or_else(|| {
                            TakerError::General("Outgoing swapcoin missing funding_tx".to_string())
                        })?;
                        wallet.send_tx(funding_tx).map_err(|e| {
                            TakerError::General(format!("Failed to broadcast funding tx: {:?}", e))
                        })?;
                    }
                    wallet.save_to_disk()?;
                }
                // Funding txs are now on-chain — mark the phase transition.
                // SP4-L: Point of no return. Persist nonces needed for legacy recovery.
                self.swap_state_mut()?.makers[maker_idx]
                    .legacy_exchange_mut()?
                    .prev_funding_broadcast = true;
                self.swap_state_mut()?.phase = super::unified_api::SwapPhase::FundsBroadcast;
                self.persist_swap(super::swap_tracker::SwapPhase::FundsBroadcast)?;
                let funding_txids: Vec<_> = self
                    .swap_state()?
                    .outgoing_swapcoins
                    .iter()
                    .filter_map(|sc| sc.funding_tx.as_ref().map(|tx| tx.compute_txid()))
                    .collect();
                self.net_wait_for_confirmation(&funding_txids, None)?;
                self.swap_state_mut()?.makers[maker_idx]
                    .legacy_exchange_mut()?
                    .prev_funding_confirmed = true;
                self.persist_progress()?;

                // Register outgoing funding outpoints as sentinels with the breach detector.
                // These are the inputs to our contract txs. If spent, a contract was broadcast.
                let outpoints: Vec<OutPoint> = self
                    .swap_state()?
                    .outgoing_swapcoins
                    .iter()
                    .map(|sc| sc.contract_tx.input[0].previous_output)
                    .collect();
                if let Some(ref detector) = self.breach_detector {
                    detector.add_sentinels(&self.watch_service, &outpoints);
                }
            }

            log::info!("Sending ProofOfFunding to maker {}", maker_idx);

            let (
                next_multisig_pubkeys,
                next_multisig_nonces,
                next_hashlock_pubkeys,
                next_hashlock_nonces,
            ) = if !is_last_peer {
                let next_maker = &self.swap_state()?.makers[maker_idx + 1];
                let tweakable_point = next_maker.tweakable_point.ok_or_else(|| {
                    TakerError::General(format!("Maker {} missing tweakable_point", maker_idx + 1))
                })?;
                generate_maker_keys(&tweakable_point, 1)?
            } else {
                // Last hop: taker is the next peer, generate our own keys
                let (multisig_pubkey, multisig_privkey) = generate_keypair();
                let (hashlock_pubkey, hashlock_privkey) = generate_keypair();
                let nonce = SecretKey::new(&mut OsRng);
                taker_multisig_privkeys = Some(vec![multisig_privkey]);
                taker_hashlock_privkeys = Some(vec![hashlock_privkey]);
                (
                    vec![multisig_pubkey],
                    vec![nonce],
                    vec![hashlock_pubkey],
                    vec![nonce],
                )
            };

            let (receivers_contract_txs, senders_contract_txs_info) = self
                .exchange_send_proof_of_funding(
                    &mut stream,
                    &swap_id,
                    &funding_txs,
                    &contract_redeemscripts,
                    &multisig_redeemscripts,
                    &multisig_nonces,
                    &hashlock_nonces,
                    &next_multisig_pubkeys,
                    &next_hashlock_pubkeys,
                    &next_multisig_nonces,
                    &next_hashlock_nonces,
                    outgoing_locktime,
                )?;
            {
                let exch = self.swap_state_mut()?.makers[maker_idx].legacy_exchange_mut()?;
                exch.proof_of_funding_sent = true;
                exch.maker_contracts_received = true;
            }
            self.persist_progress()?;

            let senders_sigs = if is_last_peer {
                log::info!("Signing sender contracts (we are last peer)");
                let privkeys = taker_multisig_privkeys.as_ref().ok_or_else(|| {
                    TakerError::General("Missing taker multisig privkeys for last hop".to_string())
                })?;
                senders_contract_txs_info
                    .iter()
                    .zip(privkeys.iter().cycle())
                    .map(|(info, privkey)| {
                        sign_contract_tx(
                            &info.contract_tx,
                            &info.multisig_redeemscript,
                            info.funding_amount,
                            privkey,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?
            } else {
                log::info!("Requesting sender signatures from next maker");
                let next_maker_address = self.swap_state()?.makers[maker_idx + 1]
                    .offer_and_address
                    .address
                    .to_string();
                let mut next_stream = self.net_connect(&next_maker_address)?;
                self.net_handshake(&mut next_stream)?;

                let hashvalue = Hash160::hash(&self.swap_state()?.preimage);
                let next_locktime = super::unified_api::REFUND_LOCKTIME_BASE
                    + super::unified_api::REFUND_LOCKTIME_STEP
                        * (maker_count - maker_idx - 2) as u16;

                let sigs = self.exchange_req_sender_sigs_forwarded(
                    &mut next_stream,
                    &swap_id,
                    &senders_contract_txs_info,
                    hashvalue,
                    next_locktime,
                )?;
                // Next maker provided sender sigs for current maker's outgoing.
                let next_exch = self.swap_state_mut()?.makers[maker_idx + 1].legacy_exchange_mut()?;
                next_exch.sender_sigs_requested = true;
                next_exch.sender_sigs_received = true;
                sigs
            };
            self.swap_state_mut()?.makers[maker_idx]
                .legacy_exchange_mut()?
                .next_maker_sigs_obtained = true;

            let receivers_sigs = if is_first_peer {
                log::info!("Signing receiver contracts (we are first peer)");
                receivers_contract_txs
                    .iter()
                    .zip(self.swap_state()?.outgoing_swapcoins.iter())
                    .map(|(rx_tx, outgoing)| {
                        let privkey = outgoing.my_privkey.ok_or(
                            crate::protocol::error::ProtocolError::General(
                                "Outgoing swapcoin missing my_privkey for signing",
                            ),
                        )?;
                        let rs = outgoing.contract_redeemscript.as_ref().ok_or(
                            crate::protocol::error::ProtocolError::General(
                                "Outgoing swapcoin missing contract_redeemscript for signing",
                            ),
                        )?;
                        sign_contract_tx(rx_tx, rs, outgoing.funding_amount, &privkey)
                    })
                    .collect::<Result<Vec<_>, _>>()?
            } else {
                log::info!("Requesting receiver signatures from previous maker");
                // For subsequent hops, request from previous maker
                let prev_maker_address = self.swap_state()?.makers[maker_idx - 1]
                    .offer_and_address
                    .address
                    .to_string();
                let mut prev_stream = self.net_connect(&prev_maker_address)?;
                self.net_handshake(&mut prev_stream)?;

                self.exchange_req_receiver_sigs(
                    &mut prev_stream,
                    &swap_id,
                    &receivers_contract_txs,
                    prev_senders_info.as_ref().unwrap(),
                )?
            };
            self.swap_state_mut()?.makers[maker_idx]
                .legacy_exchange_mut()?
                .prev_maker_sigs_obtained = true;

            log::info!(
                "Sending RespContractSigsForRecvrAndSender to maker {}",
                maker_idx
            );
            self.exchange_send_combined_sigs(&mut stream, &swap_id, receivers_sigs, senders_sigs)?;
            self.swap_state_mut()?.makers[maker_idx]
                .legacy_exchange_mut()?
                .combined_sigs_sent = true;
            self.persist_progress()?;

            // Store this maker's outgoing info for next hop
            prev_senders_info = Some(senders_contract_txs_info.clone());

            // For non-first hops, the taker doesn't own these contracts — track as watch-only
            if !is_first_peer {
                let watchonly_coins: Vec<WatchOnlySwapCoin> = senders_contract_txs_info
                    .iter()
                    .map(|info| {
                        let (pubkey1, pubkey2) =
                            read_pubkeys_from_multisig_redeemscript(&info.multisig_redeemscript)?;
                        Ok(WatchOnlySwapCoin::new_legacy(
                            pubkey1,
                            pubkey2,
                            info.contract_tx.clone(),
                            info.contract_redeemscript.clone(),
                            info.funding_amount,
                        ))
                    })
                    .collect::<Result<Vec<_>, TakerError>>()?;

                let swap_id = self.swap_state()?.id.clone();
                {
                    let mut wallet = self.write_wallet()?;
                    wallet.add_unified_watchonly_swapcoins(&swap_id, watchonly_coins.clone());
                    wallet.save_to_disk()?;
                }
                self.swap_state_mut()?
                    .watchonly_swapcoins
                    .extend(watchonly_coins);
                self.swap_state_mut()?.makers[maker_idx]
                    .legacy_exchange_mut()?
                    .watchonly_created = true;
            }

            // Wait for this maker's funding to be broadcast and confirmed
            log::info!(
                "Waiting for maker {}'s funding to be confirmed...",
                maker_idx
            );
            // The maker broadcasts after receiving sigs - give it a moment then wait
            std::thread::sleep(MAKER_BROADCAST_DELAY);

            // Wait for the maker's specific funding txs to be confirmed
            let maker_funding_txids: Vec<Txid> = senders_contract_txs_info
                .iter()
                .map(|i| i.funding_tx.compute_txid())
                .collect();

            self.net_wait_for_confirmation(&maker_funding_txids, self.breach_detector.as_ref())?;
            self.swap_state_mut()?.makers[maker_idx]
                .legacy_exchange_mut()?
                .maker_funding_confirmed = true;

            // Register this maker's funding outpoints as sentinels for subsequent waits.
            let maker_outpoints: Vec<OutPoint> = senders_contract_txs_info
                .iter()
                .map(|info| info.contract_tx.input[0].previous_output)
                .collect();
            if let Some(ref detector) = self.breach_detector {
                detector.add_sentinels(&self.watch_service, &maker_outpoints);
            }

            // This maker's funding is the previous hop for the next maker.
            if maker_idx + 1 < maker_count {
                let next_exch = self.swap_state_mut()?.makers[maker_idx + 1].legacy_exchange_mut()?;
                next_exch.prev_funding_broadcast = true;
                next_exch.prev_funding_confirmed = true;
            }
            self.persist_progress()?;

            log::info!("Maker {} processed successfully", maker_idx);
        }

        // Create incoming swapcoins from the last maker's sender contract info.
        // These represent the taker's receivable coins from the last hop.
        if let Some(last_senders_info) = &prev_senders_info {
            let multisig_privkeys = taker_multisig_privkeys.ok_or_else(|| {
                TakerError::General(
                    "Missing taker multisig privkeys for incoming swapcoins".to_string(),
                )
            })?;
            let hashlock_privkeys = taker_hashlock_privkeys.ok_or_else(|| {
                TakerError::General(
                    "Missing taker hashlock privkeys for incoming swapcoins".to_string(),
                )
            })?;

            let secp = Secp256k1::new();
            for (info, (multisig_privkey, hashlock_privkey)) in last_senders_info.iter().zip(
                multisig_privkeys
                    .iter()
                    .cycle()
                    .zip(hashlock_privkeys.iter().cycle()),
            ) {
                // Extract the maker's pubkey from the multisig redeemscript
                let (pubkey1, pubkey2) =
                    crate::protocol::contract::read_pubkeys_from_multisig_redeemscript(
                        &info.multisig_redeemscript,
                    )?;
                let my_pubkey = PublicKey {
                    compressed: true,
                    inner: secp256k1::PublicKey::from_secret_key(&secp, multisig_privkey),
                };
                let other_pubkey = if pubkey1 == my_pubkey {
                    pubkey2
                } else {
                    pubkey1
                };

                let mut incoming = IncomingSwapCoin::new_legacy(
                    *multisig_privkey,
                    other_pubkey,
                    info.contract_tx.clone(),
                    info.contract_redeemscript.clone(),
                    *hashlock_privkey,
                    info.funding_amount,
                );
                incoming.set_preimage(self.swap_state()?.preimage);
                self.swap_state_mut()?.incoming_swapcoins.push(incoming);
            }

            log::info!(
                "Created {} incoming swapcoins from last maker",
                self.swap_state()?.incoming_swapcoins.len()
            );

            // Request the last maker's contract signature on the incoming contracts.
            // This allows the taker to broadcast the incoming contract tx during recovery
            // (for hashlock spending) without depending on the maker.
            let last_maker_address = self.swap_state()?.makers[maker_count - 1]
                .offer_and_address
                .address
                .to_string();
            log::info!(
                "Requesting receiver contract sigs from last maker: {}",
                last_maker_address
            );
            let mut last_maker_stream = self.net_connect(&last_maker_address)?;
            self.net_handshake(&mut last_maker_stream)?;

            let incoming_contract_txs: Vec<Transaction> = last_senders_info
                .iter()
                .map(|info| info.contract_tx.clone())
                .collect();

            let receiver_sigs = self.exchange_req_receiver_sigs(
                &mut last_maker_stream,
                &swap_id,
                &incoming_contract_txs,
                last_senders_info,
            )?;

            // Store the maker's signatures on the incoming swapcoins
            let swap = self.swap_state_mut()?;
            for (incoming, sig) in swap.incoming_swapcoins.iter_mut().zip(receiver_sigs.iter()) {
                incoming.others_contract_sig = Some(*sig);
            }
            log::info!(
                "Stored {} receiver contract signatures on incoming swapcoins",
                receiver_sigs.len()
            );
        }

        // SP6-L: All makers responded, incoming swapcoins created.
        // Breach detector keeps running through finalization.
        self.persist_swap(super::swap_tracker::SwapPhase::ContractsExchanged)?;

        log::info!("Multi-hop Legacy swap contract exchange completed");
        Ok(())
    }

    /// Request contract signatures for sender from a maker.
    fn exchange_req_sender_sigs(
        &self,
        stream: &mut TcpStream,
        swap_id: &str,
        outgoing_swapcoins: &[OutgoingSwapCoin],
        locktime: u16,
    ) -> Result<Vec<bitcoin::ecdsa::Signature>, TakerError> {
        let secp = Secp256k1::new();

        // Use the correct nonces from swap state — these are the nonces that
        // were used to derive the maker's multisig/hashlock pubkeys during
        // setup_funding(). The maker needs them to derive the matching private
        // keys for signing.
        let swap_state = self.swap_state()?;
        let multisig_nonce = swap_state
            .multisig_nonces
            .first()
            .ok_or_else(|| TakerError::General("No multisig nonce in swap state".to_string()))?;
        let hashlock_nonce = swap_state
            .hashlock_nonces
            .first()
            .ok_or_else(|| TakerError::General("No hashlock nonce in swap state".to_string()))?;

        let txs_info: Vec<ContractTxInfoForSender> = outgoing_swapcoins
            .iter()
            .map(|swapcoin| {
                let timelock_pubkey = PublicKey {
                    compressed: true,
                    inner: secp256k1::PublicKey::from_secret_key(&secp, &swapcoin.timelock_privkey),
                };

                let my_pub = swapcoin.my_pubkey.ok_or_else(|| {
                    TakerError::General("Outgoing swapcoin missing my_pubkey".to_string())
                })?;
                let other_pub = swapcoin.other_pubkey.ok_or_else(|| {
                    TakerError::General("Outgoing swapcoin missing other_pubkey".to_string())
                })?;
                let multisig_redeemscript = create_multisig_redeemscript(&my_pub, &other_pub);

                Ok(ContractTxInfoForSender {
                    multisig_nonce: *multisig_nonce,
                    hashlock_nonce: *hashlock_nonce,
                    timelock_pubkey,
                    senders_contract_tx: swapcoin.contract_tx.clone(),
                    multisig_redeemscript,
                    funding_input_value: swapcoin.funding_amount,
                })
            })
            .collect::<Result<Vec<_>, TakerError>>()?;

        let hashvalue = Hash160::hash(&self.swap_state()?.preimage);

        let req = ReqContractSigsForSender {
            id: swap_id.to_string(),
            txs_info,
            hashvalue,
            locktime,
        };

        send_message(stream, &TakerToMakerMessage::ReqContractSigsForSender(req))?;

        let msg_bytes = read_message(stream)?;
        let msg: MakerToTakerMessage = serde_cbor::from_slice(&msg_bytes)?;

        match msg {
            MakerToTakerMessage::RespContractSigsForSender(resp) => {
                if resp.sigs.len() != outgoing_swapcoins.len() {
                    return Err(TakerError::General(format!(
                        "Wrong number of signatures: expected {}, got {}",
                        outgoing_swapcoins.len(),
                        resp.sigs.len()
                    )));
                }
                log::info!(
                    "Received {} sender contract signatures from maker",
                    resp.sigs.len()
                );
                Ok(resp.sigs)
            }
            other => Err(TakerError::General(format!(
                "Unexpected message: expected RespContractSigsForSender, got {:?}",
                other
            ))),
        }
    }

    /// Send proof of funding and receive ReqContractSigsAsRecvrAndSender.
    #[allow(clippy::type_complexity)]
    #[allow(clippy::too_many_arguments)]
    fn exchange_send_proof_of_funding(
        &self,
        stream: &mut TcpStream,
        swap_id: &str,
        funding_txs: &[Transaction],
        contract_redeemscripts: &[ScriptBuf],
        multisig_redeemscripts: &[ScriptBuf],
        multisig_nonces: &[SecretKey],
        hashlock_nonces: &[SecretKey],
        next_multisig_pubkeys: &[PublicKey],
        next_hashlock_pubkeys: &[PublicKey],
        next_multisig_nonces: &[SecretKey],
        next_hashlock_nonces: &[SecretKey],
        refund_locktime: u16,
    ) -> Result<(Vec<Transaction>, Vec<SenderContractTxInfo>), TakerError> {
        let confirmed_funding_txes: Vec<FundingTxInfo> = funding_txs
            .iter()
            .zip(multisig_redeemscripts.iter())
            .zip(contract_redeemscripts.iter())
            .zip(multisig_nonces.iter())
            .zip(hashlock_nonces.iter())
            .map(
                |((((funding_tx, multisig_rs), contract_rs), &multisig_nonce), &hashlock_nonce)| {
                    FundingTxInfo {
                        funding_tx: funding_tx.clone(),
                        funding_tx_merkleproof: String::new(), // TODO: Get actual merkle proof
                        multisig_redeemscript: multisig_rs.clone(),
                        multisig_nonce,
                        contract_redeemscript: contract_rs.clone(),
                        hashlock_nonce,
                    }
                },
            )
            .collect();

        let next_coinswap_info: Vec<NextHopInfo> = next_multisig_pubkeys
            .iter()
            .zip(next_hashlock_pubkeys.iter())
            .zip(next_multisig_nonces.iter())
            .zip(next_hashlock_nonces.iter())
            .map(
                |(
                    ((&next_multisig_pubkey, &next_hashlock_pubkey), &next_multisig_nonce),
                    &next_hashlock_nonce,
                )| NextHopInfo {
                    next_multisig_pubkey,
                    next_hashlock_pubkey,
                    next_multisig_nonce,
                    next_hashlock_nonce,
                },
            )
            .collect();

        let pof = ProofOfFunding {
            id: swap_id.to_string(),
            confirmed_funding_txes,
            next_coinswap_info,
            refund_locktime,
            contract_feerate: MIN_FEE_RATE,
        };

        send_message(stream, &TakerToMakerMessage::ProofOfFunding(pof))?;

        let msg_bytes = read_message(stream)?;
        let msg: MakerToTakerMessage = serde_cbor::from_slice(&msg_bytes)?;

        match msg {
            MakerToTakerMessage::ReqContractSigsAsRecvrAndSender(req) => {
                log::info!(
                    "Received ReqContractSigsAsRecvrAndSender: {} receivers, {} senders",
                    req.receivers_contract_txs.len(),
                    req.senders_contract_txs_info.len()
                );
                Ok((req.receivers_contract_txs, req.senders_contract_txs_info))
            }
            other => Err(TakerError::General(format!(
                "Unexpected message: expected ReqContractSigsAsRecvrAndSender, got {:?}",
                other
            ))),
        }
    }

    /// Send collected signatures for both receiver and sender contracts (Legacy protocol).
    fn exchange_send_combined_sigs(
        &self,
        stream: &mut TcpStream,
        swap_id: &str,
        receivers_sigs: Vec<bitcoin::ecdsa::Signature>,
        senders_sigs: Vec<bitcoin::ecdsa::Signature>,
    ) -> Result<(), TakerError> {
        let resp = RespContractSigsForRecvrAndSender {
            id: swap_id.to_string(),
            receivers_sigs,
            senders_sigs,
        };

        send_message(
            stream,
            &TakerToMakerMessage::RespContractSigsForRecvrAndSender(resp),
        )?;

        log::info!(
            "Sent RespContractSigsForRecvrAndSender for swap {}",
            swap_id
        );
        Ok(())
    }

    /// Request sender contract signatures using SenderContractTxInfo.
    fn exchange_req_sender_sigs_forwarded(
        &self,
        stream: &mut TcpStream,
        swap_id: &str,
        senders_info: &[SenderContractTxInfo],
        hashvalue: Hash160,
        locktime: u16,
    ) -> Result<Vec<bitcoin::ecdsa::Signature>, TakerError> {
        // Build ContractTxInfoForSender from SenderContractTxInfo
        let txs_info: Vec<ContractTxInfoForSender> = senders_info
            .iter()
            .map(|info| ContractTxInfoForSender {
                multisig_nonce: info.multisig_nonce,
                hashlock_nonce: info.hashlock_nonce,
                timelock_pubkey: info.timelock_pubkey,
                senders_contract_tx: info.contract_tx.clone(),
                multisig_redeemscript: info.multisig_redeemscript.clone(),
                funding_input_value: info.funding_amount,
            })
            .collect();

        let req = ReqContractSigsForSender {
            id: swap_id.to_string(),
            txs_info,
            hashvalue,
            locktime,
        };

        send_message(stream, &TakerToMakerMessage::ReqContractSigsForSender(req))?;

        let msg_bytes = read_message(stream)?;
        let msg: MakerToTakerMessage = serde_cbor::from_slice(&msg_bytes)?;

        match msg {
            MakerToTakerMessage::RespContractSigsForSender(resp) => {
                log::info!("Received {} sender signatures", resp.sigs.len());
                Ok(resp.sigs)
            }
            other => Err(TakerError::General(format!(
                "Unexpected message: expected RespContractSigsForSender, got {:?}",
                other
            ))),
        }
    }

    /// Request receiver signatures from previous maker.
    fn exchange_req_receiver_sigs(
        &self,
        stream: &mut TcpStream,
        swap_id: &str,
        receivers_txs: &[Transaction],
        prev_senders_info: &[SenderContractTxInfo],
    ) -> Result<Vec<bitcoin::ecdsa::Signature>, TakerError> {
        // Build ContractTxInfoForRecvr for each receiver contract
        let txs: Vec<ContractTxInfoForRecvr> = receivers_txs
            .iter()
            .zip(prev_senders_info.iter())
            .map(|(tx, info)| ContractTxInfoForRecvr {
                contract_tx: tx.clone(),
                multisig_redeemscript: info.multisig_redeemscript.clone(),
            })
            .collect();

        let req = ReqContractSigsForRecvr {
            id: swap_id.to_string(),
            txs,
        };

        send_message(stream, &TakerToMakerMessage::ReqContractSigsForRecvr(req))?;

        let msg_bytes = read_message(stream)?;
        let msg: MakerToTakerMessage = serde_cbor::from_slice(&msg_bytes)?;

        match msg {
            MakerToTakerMessage::RespContractSigsForRecvr(resp) => {
                log::info!("Received {} receiver signatures", resp.sigs.len());
                Ok(resp.sigs)
            }
            other => Err(TakerError::General(format!(
                "Unexpected message: expected RespContractSigsForRecvr, got {:?}",
                other
            ))),
        }
    }
}

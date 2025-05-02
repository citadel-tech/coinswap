//! Collection of all message handlers for a Maker.
//!
//! Implements the logic for message handling based on the current connection state.
//! Exposes the main function [handle_message] to process incoming messages and generate outgoing messages.
//! Also includes handlers for specific messages such as contract signatures, proof of funding, hash preimage, and private key handover.
//! Manages wallet state, incoming and outgoing swap coins, and special behaviors defined for the Maker.
//! The file includes functions to validate and sign contract transactions, verify proof of funding, and handle unexpected recovery scenarios.
//! Implements the core functionality for a Maker in a Bitcoin coinswap protocol.

use std::{collections::HashMap, sync::Arc, time::Instant};

use bitcoin::{
    hashes::Hash,
    secp256k1::{self, Secp256k1},
    Amount, OutPoint, PublicKey, Transaction, Txid,
};

use super::{
    api::{
        recover_from_swap, ConnectionState, ExpectedMessage, Maker, MakerBehavior,
        AMOUNT_RELATIVE_FEE_PCT, BASE_FEE, MIN_CONTRACT_REACTION_TIME, TIME_RELATIVE_FEE_PCT,
    },
    error::MakerError,
};

use crate::{
    protocol::{
        contract::{
            calculate_coinswap_fee, create_receivers_contract_tx, find_funding_output_index,
            read_hashvalue_from_contract, read_pubkeys_from_multisig_redeemscript,
        },
        error::ProtocolError,
        messages::{
            ContractSigsAsRecvrAndSender, ContractSigsForRecvr, ContractSigsForRecvrAndSender,
            ContractSigsForSender, HashPreimage, MakerHello, MakerToTakerMessage, MultisigPrivkey,
            Offer, PrivKeyHandover, ProofOfFunding, ReqContractSigsForRecvr,
            ReqContractSigsForSender, SenderContractTxInfo, TakerToMakerMessage,
        },
        Hash160,
    },
    utill::{DEFAULT_TX_FEE_RATE, REQUIRED_CONFIRMS},
    wallet::{IncomingSwapCoin, SwapCoin, WalletError, WalletSwapCoin},
};

/// The Global Handle Message function. Takes in a [`Arc<Maker>`] and handles messages
/// according to a [ConnectionState].
pub(crate) fn handle_message(
    maker: &Arc<Maker>,
    connection_state: &mut ConnectionState,
    message: TakerToMakerMessage,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    // If taker is waiting for funding confirmation, reset the timer.
    if let TakerToMakerMessage::WaitingFundingConfirmation(id) = &message {
        log::info!(
            "[{}] Taker is waiting for funding confirmation. Resetting timer.",
            maker.config.network_port
        );
        maker
            .ongoing_swap_state
            .lock()?
            .entry(id.clone())
            .and_modify(|(_, timer)| *timer = Instant::now());
        return Ok(None);
    }

    let outgoing_message = match connection_state.allowed_message {
        ExpectedMessage::TakerHello => {
            if let TakerToMakerMessage::TakerHello(m) = message {
                if m.protocol_version_min != 1 && m.protocol_version_max != 1 {
                    return Err(ProtocolError::WrongMessage {
                        expected: "Only protocol version 1 is allowed".to_string(),
                        received: format!(
                            "min/max version  = {}/{}",
                            m.protocol_version_min, m.protocol_version_max
                        ),
                    }
                    .into());
                }
                connection_state.allowed_message = ExpectedMessage::NewlyConnectedTaker;
                let reply = MakerToTakerMessage::MakerHello(MakerHello {
                    protocol_version_min: 1,
                    protocol_version_max: 1,
                });
                Some(reply)
            } else {
                return Err(MakerError::UnexpectedMessage {
                    expected: "TakerHello".to_string(),
                    got: format!("{message}"),
                });
            }
        }
        ExpectedMessage::NewlyConnectedTaker => match message {
            TakerToMakerMessage::ReqGiveOffer(_) => {
                let (tweakable_point, max_size) = {
                    let wallet_reader = maker.wallet.read()?;
                    let max_size = wallet_reader.store.offer_maxsize;
                    let tweakable_point = wallet_reader.get_tweakable_keypair()?.1;
                    (tweakable_point, max_size)
                };
                connection_state.allowed_message = ExpectedMessage::ReqContractSigsForSender;
                let fidelity = maker.highest_fidelity_proof.read()?;
                let fidelity = fidelity.as_ref().expect("proof expected");
                Some(MakerToTakerMessage::RespOffer(Box::new(Offer {
                    base_fee: BASE_FEE,
                    amount_relative_fee_pct: AMOUNT_RELATIVE_FEE_PCT,
                    time_relative_fee_pct: TIME_RELATIVE_FEE_PCT,
                    required_confirms: REQUIRED_CONFIRMS,
                    minimum_locktime: MIN_CONTRACT_REACTION_TIME,
                    max_size,
                    min_size: maker.config.min_swap_amount,
                    tweakable_point,
                    fidelity: fidelity.clone(),
                })))
            }
            TakerToMakerMessage::ReqContractSigsForSender(message) => {
                connection_state.allowed_message = ExpectedMessage::ProofOfFunding;
                Some(maker.handle_req_contract_sigs_for_sender(message)?)
            }
            TakerToMakerMessage::RespProofOfFunding(proof) => {
                connection_state.allowed_message =
                    ExpectedMessage::ProofOfFundingORContractSigsForRecvrAndSender;
                Some(maker.handle_proof_of_funding(connection_state, proof)?)
            }
            TakerToMakerMessage::ReqContractSigsForRecvr(message) => {
                connection_state.allowed_message = ExpectedMessage::HashPreimage;
                Some(maker.handle_req_contract_sigs_for_recvr(message)?)
            }
            TakerToMakerMessage::RespHashPreimage(message) => {
                connection_state.allowed_message = ExpectedMessage::PrivateKeyHandover;
                Some(maker.handle_hash_preimage(message)?)
            }
            _ => {
                log::info!("Newlyconnected taker stage message: {message:?} ");
                return Err(MakerError::General(
                    "Unexpected Newly Connected Taker message",
                ));
            }
        },
        ExpectedMessage::ReqContractSigsForSender => {
            if let TakerToMakerMessage::ReqContractSigsForSender(message) = message {
                connection_state.allowed_message = ExpectedMessage::ProofOfFunding;
                Some(maker.handle_req_contract_sigs_for_sender(message)?)
            } else {
                return Err(MakerError::UnexpectedMessage {
                    expected: "ReqContractSigsForSender".to_string(),
                    got: format!("{message}"),
                });
            }
        }
        ExpectedMessage::ProofOfFunding => {
            if let TakerToMakerMessage::RespProofOfFunding(proof) = message {
                connection_state.allowed_message =
                    ExpectedMessage::ProofOfFundingORContractSigsForRecvrAndSender;
                Some(maker.handle_proof_of_funding(connection_state, proof)?)
            } else {
                return Err(MakerError::UnexpectedMessage {
                    expected: "Proof OF Funding".to_string(),
                    got: format!("{message}"),
                });
            }
        }
        ExpectedMessage::ProofOfFundingORContractSigsForRecvrAndSender => {
            match message {
                TakerToMakerMessage::RespProofOfFunding(proof) => {
                    connection_state.allowed_message =
                        ExpectedMessage::ProofOfFundingORContractSigsForRecvrAndSender;
                    Some(maker.handle_proof_of_funding(connection_state, proof)?)
                }
                TakerToMakerMessage::RespContractSigsForRecvrAndSender(message) => {
                    // Nothing to send. Maker now creates and broadcasts his funding Txs
                    connection_state.allowed_message = ExpectedMessage::ReqContractSigsForRecvr;
                    maker.handle_contract_sigs_for_recvr_and_sender(connection_state, message)?;
                    if let MakerBehavior::BroadcastContractAfterSetup = maker.behavior {
                        unexpected_recovery(maker.clone())?;
                        return Err(maker.behavior.into());
                    } else {
                        None
                    }
                }
                _ => {
                    return Err(MakerError::General(
                        "Expected proof of funding or sender's and receiver's contract signatures",
                    ));
                }
            }
        }
        ExpectedMessage::ReqContractSigsForRecvr => {
            if let TakerToMakerMessage::ReqContractSigsForRecvr(message) = message {
                connection_state.allowed_message = ExpectedMessage::HashPreimage;
                Some(maker.handle_req_contract_sigs_for_recvr(message)?)
            } else {
                return Err(MakerError::General(
                    "Expected receiver's contract transaction",
                ));
            }
        }
        ExpectedMessage::HashPreimage => {
            if let TakerToMakerMessage::RespHashPreimage(message) = message {
                connection_state.allowed_message = ExpectedMessage::PrivateKeyHandover;
                Some(maker.handle_hash_preimage(message)?)
            } else {
                return Err(MakerError::General("Expected hash preimage"));
            }
        }
        ExpectedMessage::PrivateKeyHandover => {
            if let TakerToMakerMessage::RespPrivKeyHandover(message) = message {
                // Nothing to send. Successfully completed swap
                maker.handle_private_key_handover(message)?;
                None
            } else {
                return Err(MakerError::General("Expected privatekey handover"));
            }
        }
    };

    Ok(outgoing_message)
}

impl Maker {
    /// This is the first message handler for the Maker. It receives a [ReqContractSigsForSender] message,
    /// checks the validity of contract transactions, and provides the signature for the sender side.
    /// This will fail if the maker doesn't have enough utxos to fund the next coinswap hop, or the contract
    /// transaction isn't valid.
    pub(crate) fn handle_req_contract_sigs_for_sender(
        &self,
        message: ReqContractSigsForSender,
    ) -> Result<MakerToTakerMessage, MakerError> {
        if let MakerBehavior::CloseAtReqContractSigsForSender = self.behavior {
            return Err(self.behavior.into());
        }

        // Verify and sign the contract transaction, and check the function definition for all the checks.
        let sigs = self.verify_and_sign_contract_tx(&message)?;

        let funding_txids = message
            .txs_info
            .iter()
            .map(|txinfo| txinfo.senders_contract_tx.input[0].previous_output.txid)
            .collect::<Vec<_>>();

        let total_funding_amount = message.txs_info.iter().fold(0u64, |acc, txinfo| {
            acc + txinfo.funding_input_value.to_sat()
        });

        log::info!(
            "[{}] Total Funding Amount = {} | Funding Txids = {:?}",
            self.config.network_port,
            Amount::from_sat(total_funding_amount),
            funding_txids
        );

        let max_size = self.wallet.read()?.store.offer_maxsize;
        if total_funding_amount >= self.config.min_swap_amount && total_funding_amount <= max_size {
            Ok(MakerToTakerMessage::RespContractSigsForSender(
                ContractSigsForSender { sigs },
            ))
        } else {
            log::error!(
                "Funding amount not within min/max limit, min {}, max {}",
                self.config.min_swap_amount,
                max_size
            );
            Err(MakerError::General("Not enough funds"))
        }
    }

    /// Validates the [ProofOfFunding] message, initiate the next hop,
    /// and create the `[ReqContractSigsAsRecvrAndSender`\] message.
    pub(crate) fn handle_proof_of_funding(
        &self,
        connection_state: &mut ConnectionState,
        message: ProofOfFunding,
    ) -> Result<MakerToTakerMessage, MakerError> {
        if let MakerBehavior::CloseAtProofOfFunding = self.behavior {
            return Err(self.behavior.into());
        }

        // Basic verification of ProofOfFunding Message.
        // Check function definition for all the checks performed.
        let hashvalue = self.verify_proof_of_funding(&message)?;
        log::info!(
            "[{}] Validated Proof of Funding of receiving swap. Adding Incoming Swaps.",
            self.config.network_port
        );

        // Import transactions and addresses into Bitcoin core's wallet.
        // Add IncomingSwapcoin to Maker's Wallet
        for funding_info in &message.confirmed_funding_txes {
            let (pubkey1, pubkey2) =
                read_pubkeys_from_multisig_redeemscript(&funding_info.multisig_redeemscript)?;

            let funding_output_index = find_funding_output_index(funding_info)?;
            let funding_output = funding_info
                .funding_tx
                .output
                .get(funding_output_index as usize)
                .expect("funding output expected at this index");

            self.wallet.write()?.sync_no_fail();

            let receiver_contract_tx = create_receivers_contract_tx(
                OutPoint {
                    txid: funding_info.funding_tx.compute_txid(),
                    vout: funding_output_index,
                },
                funding_output.value,
                &funding_info.contract_redeemscript,
                Amount::from_sat(message.contract_feerate),
            )?;

            let (tweakable_privkey, _) = self.wallet.read()?.get_tweakable_keypair()?;
            let multisig_privkey =
                tweakable_privkey.add_tweak(&funding_info.multisig_nonce.into())?;

            let multisig_pubkey = PublicKey {
                compressed: true,
                inner: secp256k1::PublicKey::from_secret_key(&Secp256k1::new(), &multisig_privkey),
            };

            let other_pubkey = if multisig_pubkey == pubkey1 {
                pubkey2
            } else {
                pubkey1
            };

            let hashlock_privkey =
                tweakable_privkey.add_tweak(&funding_info.hashlock_nonce.into())?;

            // Taker can send the same funding transactions twice. Happens when one maker in the
            // path fails. Only add it if it didn't already exist.
            let incoming_swapcoin = IncomingSwapCoin::new(
                multisig_privkey,
                other_pubkey,
                receiver_contract_tx.clone(),
                funding_info.contract_redeemscript.clone(),
                hashlock_privkey,
                funding_output.value,
            )?;
            if !connection_state
                .incoming_swapcoins
                .contains(&incoming_swapcoin)
            {
                connection_state.incoming_swapcoins.push(incoming_swapcoin);
            }
        }

        // Calculate output amounts for the next hop
        let incoming_amount = message
            .confirmed_funding_txes
            .iter()
            .try_fold(0u64, |acc, fi| {
                let index = find_funding_output_index(fi)?;
                let txout = fi
                    .funding_tx
                    .output
                    .get(index as usize)
                    .expect("output at index expected");
                Ok::<_, MakerError>(acc + txout.value.to_sat())
            })?;

        let calc_coinswap_fees = calculate_coinswap_fee(
            incoming_amount,
            message.refund_locktime,
            BASE_FEE,
            AMOUNT_RELATIVE_FEE_PCT,
            TIME_RELATIVE_FEE_PCT,
        );

        // NOTE: The `contract_feerate` currently represents the hardcoded `MINER_FEE` of a transaction, not the fee rate.
        // This will remain unchanged to avoid modifying the structure of the [ProofOfFunding] message.
        // Once issue https://github.com/citadel-tech/coinswap/issues/309 is resolved,
        //`contract_feerate` will represent the actual fee rate instead of the `MINER_FEE`.
        let calc_funding_tx_fees =
            message.contract_feerate * (message.next_coinswap_info.len() as u64);

        // Check for overflow. If this happens, hard error.
        // This can happen if the fee_rate for funding tx is very high and incoming_amount is very low.
        // TODO: Ensure at Taker protocol that this never happens.
        let outgoing_amount = if let Some(a) =
            incoming_amount.checked_sub(calc_coinswap_fees + calc_funding_tx_fees)
        {
            a
        } else {
            return Err(MakerError::General(
                "Fatal Error! Total swap fee is more than the swap amount. Failing the swap.",
            ));
        };

        // Create outgoing coinswap of the next hop
        let (my_funding_txes, outgoing_swapcoins, act_funding_txs_fees) = {
            self.wallet.write()?.initalize_coinswap(
                Amount::from_sat(outgoing_amount),
                &message
                    .next_coinswap_info
                    .iter()
                    .map(|next_hop| next_hop.next_multisig_pubkey)
                    .collect::<Vec<PublicKey>>(),
                &message
                    .next_coinswap_info
                    .iter()
                    .map(|next_hop| next_hop.next_hashlock_pubkey)
                    .collect::<Vec<PublicKey>>(),
                hashvalue,
                message.refund_locktime,
                Amount::from_sat(message.contract_feerate),
            )?
        };

        let act_coinswap_fees = incoming_amount
            .checked_sub(outgoing_amount + act_funding_txs_fees.to_sat())
            .expect("This should not overflow as we just checked above.");

        log::info!(
            "[{}] Prepared outgoing funding txs: {:?}.",
            self.config.network_port,
            my_funding_txes
                .iter()
                .map(|tx| tx.compute_txid())
                .collect::<Vec<_>>()
        );

        log::info!(
            "[{}] Incoming Swap Amount = {} | Outgoing Swap Amount = {} | Coinswap Fee = {} |   Refund Tx locktime (blocks) = {} | Total Funding Tx Mining Fees = {} |",
            self.config.network_port,
            Amount::from_sat(incoming_amount),
            Amount::from_sat(outgoing_amount),
            Amount::from_sat(act_coinswap_fees),
            message.refund_locktime,
            act_funding_txs_fees
        );

        connection_state.pending_funding_txes = my_funding_txes;
        connection_state.outgoing_swapcoins = outgoing_swapcoins;

        // Save things to disk after Proof of Funding is confirmed.
        {
            let mut wallet_writer = self.wallet.write()?;
            for (incoming_sc, outgoing_sc) in connection_state
                .incoming_swapcoins
                .iter()
                .zip(connection_state.outgoing_swapcoins.iter())
            {
                wallet_writer.add_incoming_swapcoin(incoming_sc);
                wallet_writer.add_outgoing_swapcoin(outgoing_sc);
            }
            wallet_writer.save_to_disk()?;
        }

        // Craft ReqContractSigsAsRecvrAndSender message to send to the Taker.
        let receivers_contract_txs = connection_state
            .incoming_swapcoins
            .iter()
            .map(|isc| isc.contract_tx.clone())
            .collect::<Vec<Transaction>>();

        let senders_contract_txs_info = connection_state
            .outgoing_swapcoins
            .iter()
            .map(|outgoing_swapcoin| {
                Ok(SenderContractTxInfo {
                    contract_tx: outgoing_swapcoin.contract_tx.clone(),
                    timelock_pubkey: outgoing_swapcoin.get_timelock_pubkey()?,
                    multisig_redeemscript: outgoing_swapcoin.get_multisig_redeemscript(),
                    funding_amount: outgoing_swapcoin.funding_amount,
                })
            })
            .collect::<Result<Vec<SenderContractTxInfo>, WalletError>>()?;

        // Update the connection state.
        self.ongoing_swap_state.lock()?.insert(
            message.id.clone(),
            (connection_state.clone(), Instant::now()),
        );

        log::info!("Connection state initialized for swap id: {}", message.id);

        Ok(MakerToTakerMessage::ReqContractSigsAsRecvrAndSender(
            ContractSigsAsRecvrAndSender {
                receivers_contract_txs,
                senders_contract_txs_info,
            },
        ))
    }

    /// Handles [ContractSigsForRecvrAndSender] message and updates the wallet state
    pub(crate) fn handle_contract_sigs_for_recvr_and_sender(
        &self,
        connection_state: &mut ConnectionState,
        message: ContractSigsForRecvrAndSender,
    ) -> Result<(), MakerError> {
        if let MakerBehavior::CloseAtContractSigsForRecvrAndSender = self.behavior {
            return Err(self.behavior.into());
        }

        if message.receivers_sigs.len() != connection_state.incoming_swapcoins.len() {
            return Err(MakerError::General(
                "Invalid number of receiver's signatures",
            ));
        }
        for (receivers_sig, incoming_swapcoin) in message
            .receivers_sigs
            .iter()
            .zip(connection_state.incoming_swapcoins.iter_mut())
        {
            incoming_swapcoin.verify_contract_tx_sig(receivers_sig)?;
            incoming_swapcoin.others_contract_sig = Some(*receivers_sig);
        }

        if message.senders_sigs.len() != connection_state.outgoing_swapcoins.len() {
            return Err(MakerError::General("Invalid number of sender's signatures"));
        }

        for (senders_sig, outgoing_swapcoin) in message
            .senders_sigs
            .iter()
            .zip(connection_state.outgoing_swapcoins.iter_mut())
        {
            outgoing_swapcoin.verify_contract_tx_sig(senders_sig)?;

            outgoing_swapcoin.others_contract_sig = Some(*senders_sig);
        }

        {
            let mut wallet_writer = self.wallet.write()?;
            for (incoming_sc, outgoing_sc) in connection_state
                .incoming_swapcoins
                .iter()
                .zip(connection_state.outgoing_swapcoins.iter())
            {
                wallet_writer.add_incoming_swapcoin(incoming_sc);
                wallet_writer.add_outgoing_swapcoin(outgoing_sc);
            }
            wallet_writer.save_to_disk()?;
        }

        let mut my_funding_txids = Vec::<Txid>::new();
        for my_funding_tx in &connection_state.pending_funding_txes {
            let txid = self.wallet.read()?.send_tx(my_funding_tx)?;

            assert_eq!(txid, my_funding_tx.compute_txid());
            my_funding_txids.push(txid);
        }
        log::info!(
            "[{}] Broadcasted funding txs: {:?}",
            self.config.network_port,
            my_funding_txids
        );

        // Update the connection state.
        self.ongoing_swap_state.lock()?.insert(
            message.id.clone(),
            (connection_state.clone(), Instant::now()),
        );

        log::info!("Connection state timer reset for swap id: {}", message.id);

        Ok(())
    }

    /// Handles [ReqContractSigsForRecvr] and returns a [MakerToTakerMessage::RespContractSigsForRecvr]
    pub(crate) fn handle_req_contract_sigs_for_recvr(
        &self,
        message: ReqContractSigsForRecvr,
    ) -> Result<MakerToTakerMessage, MakerError> {
        if let MakerBehavior::CloseAtContractSigsForRecvr = self.behavior {
            return Err(self.behavior.into());
        }

        let sigs = message
            .txs
            .iter()
            .map(|txinfo| {
                Ok(self
                    .wallet
                    .read()?
                    .find_outgoing_swapcoin(&txinfo.multisig_redeemscript)
                    .expect("Outgoing Swapcoin expected")
                    .sign_contract_tx_with_my_privkey(&txinfo.contract_tx)?)
            })
            .collect::<Result<Vec<_>, MakerError>>()?;

        Ok(MakerToTakerMessage::RespContractSigsForRecvr(
            ContractSigsForRecvr { sigs },
        ))
    }

    /// Handles a [HashPreimage] message and returns a [MakerToTakerMessage::RespPrivKeyHandover]
    pub(crate) fn handle_hash_preimage(
        &self,
        message: HashPreimage,
    ) -> Result<MakerToTakerMessage, MakerError> {
        if let MakerBehavior::CloseAtHashPreimage = self.behavior {
            return Err(self.behavior.into());
        }

        let hashvalue = Hash160::hash(&message.preimage);
        for multisig_redeemscript in &message.senders_multisig_redeemscripts {
            let mut wallet_write = self.wallet.write()?;
            let incoming_swapcoin = wallet_write
                .find_incoming_swapcoin_mut(multisig_redeemscript)
                .expect("Incoming swapcoin expected");
            if read_hashvalue_from_contract(&incoming_swapcoin.contract_redeemscript)? != hashvalue
            {
                return Err(MakerError::General("Incorrect hash preimage"));
            }
            incoming_swapcoin.hash_preimage = Some(message.preimage);
        }

        log::info!(
            "[{}] received preimage for hashvalue={}",
            self.config.network_port,
            hashvalue
        );
        let mut swapcoin_private_keys = Vec::<MultisigPrivkey>::new();

        // Send our privkey and mark the outgoing swapcoin as "done".
        for multisig_redeemscript in &message.receivers_multisig_redeemscripts {
            let mut wallet_write = self.wallet.write()?;
            let outgoing_swapcoin = wallet_write
                .find_outgoing_swapcoin_mut(multisig_redeemscript)
                .expect("outgoing swapcoin expected");
            if read_hashvalue_from_contract(&outgoing_swapcoin.contract_redeemscript)? != hashvalue
            {
                return Err(MakerError::General("Incorrect hash preimage"));
            } else {
                outgoing_swapcoin.hash_preimage.replace(message.preimage);
            }

            swapcoin_private_keys.push(MultisigPrivkey {
                multisig_redeemscript: multisig_redeemscript.clone(),
                key: outgoing_swapcoin.my_privkey,
            });
        }

        self.wallet.write()?.save_to_disk()?;
        Ok(MakerToTakerMessage::RespPrivKeyHandover(PrivKeyHandover {
            multisig_privkeys: swapcoin_private_keys,
        }))
    }

    /// Handles [PrivKeyHandover] message and updates all the coinswap wallet states and stores it to disk.
    /// This is the last step of completing a coinswap round.
    pub(crate) fn handle_private_key_handover(
        &self,
        message: PrivKeyHandover,
    ) -> Result<(), MakerError> {
        // Mark the incoming swapcoins as "done", by adding their's privkey
        for swapcoin_private_key in &message.multisig_privkeys {
            self.wallet
                .write()?
                .find_incoming_swapcoin_mut(&swapcoin_private_key.multisig_redeemscript)
                .expect("incoming swapcoin not found")
                .apply_privkey(swapcoin_private_key.key)?;
        }

        // Reset the connection state so watchtowers are not triggered.
        let mut conn_state = self.ongoing_swap_state.lock()?;
        *conn_state = HashMap::default();

        log::info!("initializing Wallet Sync.");
        {
            let mut wallet_write = self.wallet.write()?;
            wallet_write.sync()?;
            wallet_write.save_to_disk()?;
        }
        log::info!("Completed Wallet Sync.");
        log::info!("Successfully Completed Coinswap");
        Ok(())
    }
}

fn unexpected_recovery(maker: Arc<Maker>) -> Result<(), MakerError> {
    let mut lock_on_state = maker.ongoing_swap_state.lock()?;
    for (_, (state, _)) in lock_on_state.iter_mut() {
        let mut outgoings = Vec::new();
        let mut incomings = Vec::new();
        // Extract Incoming and Outgoing contracts, and timelock spends of the contract transactions.
        // fully signed.
        for (og_sc, ic_sc) in state
            .outgoing_swapcoins
            .iter()
            .zip(state.incoming_swapcoins.iter())
        {
            let contract_timelock = og_sc.get_timelock()?;
            let contract = match og_sc.get_fully_signed_contract_tx() {
                Ok(tx) => tx,
                Err(e) => {
                    log::error!(
                        "Error: {e:?} \
                        This was not supposed to happen. \
                        Kindly open an issue at https://github.com/citadel-tech/coinswap/issues."
                    );
                    maker
                        .wallet
                        .write()?
                        .remove_outgoing_swapcoin(&og_sc.get_multisig_redeemscript())?;
                    continue;
                }
            };
            let next_internal_address = &maker.wallet.read()?.get_next_internal_addresses(1)?[0];
            let time_lock_spend = maker.wallet.read()?.create_timelock_spend(
                og_sc,
                next_internal_address,
                DEFAULT_TX_FEE_RATE,
            )?;
            outgoings.push((
                (og_sc.get_multisig_redeemscript(), contract),
                (contract_timelock, time_lock_spend),
            ));
            let incoming_contract = ic_sc.get_fully_signed_contract_tx()?;
            incomings.push((ic_sc.get_multisig_redeemscript(), incoming_contract));
        }
        // Spawn a separate thread to wait for contract maturity and broadcasting timelocked.
        let maker_clone = maker.clone();
        let handle = std::thread::Builder::new()
            .name("Swap Recovery Thread".to_string())
            .spawn(move || {
                if let Err(e) = recover_from_swap(maker_clone, outgoings, incomings) {
                    log::error!("Failed to recover from swap due to: {e:?}");
                }
            })?;
        maker.thread_pool.add_thread(handle);
    }
    Ok(())
}

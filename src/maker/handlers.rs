use std::{net::IpAddr, sync::Arc, time::Instant};

use bitcoin::{
    hashes::Hash,
    secp256k1::{self, ecdsa::Signature, Secp256k1},
    Amount, OutPoint, PublicKey, Transaction, Txid,
};
use bitcoind::bitcoincore_rpc::RpcApi;

use crate::protocol::{
    messages::{MultisigPrivkey, PrivKeyHandover},
    Hash160,
};

use crate::{
    maker::maker::ExpectedMessage,
    protocol::{
        contract::{
            calculate_coinswap_fee, create_receivers_contract_tx, find_funding_output_index,
            read_contract_locktime, read_hashvalue_from_contract,
            read_pubkeys_from_multisig_redeemscript, FUNDING_TX_VBYTE_SIZE,
        },
        messages::{
            ContractSigsAsRecvrAndSender, ContractSigsForRecvr, ContractSigsForRecvrAndSender,
            ContractSigsForSender, HashPreimage, MakerToTakerMessage, Offer, ProofOfFunding,
            ReqContractSigsForRecvr, ReqContractSigsForSender, SenderContractTxInfo,
            TakerToMakerMessage,
        },
    },
    wallet::{IncomingSwapCoin, SwapCoin},
};

use super::{
    error::MakerError,
    maker::{ConnectionState, Maker, MakerBehavior},
};

/// The Global Handle Message function. Takes in a [Arc<Maker>] and handle messages
/// according to a [ConnectionState].
pub async fn handle_message(
    maker: &Arc<Maker>,
    connection_state: &mut ConnectionState,
    message: TakerToMakerMessage,
    ip: IpAddr,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    let outgoing_message = match connection_state.allowed_message {
        ExpectedMessage::TakerHello => {
            if let TakerToMakerMessage::TakerHello(_) = message {
                connection_state.allowed_message = ExpectedMessage::NewlyConnectedTaker;
                None
            } else {
                return Err(MakerError::UnexpectedMessage {
                    expected: "TakerHello".to_string(),
                    got: format!("{}", message),
                });
            }
        }
        ExpectedMessage::NewlyConnectedTaker => match message {
            TakerToMakerMessage::ReqGiveOffer(_) => {
                let (tweakable_point, max_size) = {
                    let wallet_reader = maker.wallet.read()?;
                    let max_size = wallet_reader.store.offer_maxsize;
                    let tweakable_point = wallet_reader.get_tweakable_keypair().1;
                    (tweakable_point, max_size)
                };
                connection_state.allowed_message = ExpectedMessage::ReqContractSigsForSender;
                Some(MakerToTakerMessage::RespOffer(Offer {
                    absolute_fee_sat: maker.config.absolute_fee_sats,
                    amount_relative_fee_ppb: maker.config.amount_relative_fee_ppb,
                    time_relative_fee_ppb: maker.config.time_relative_fee_ppb,
                    required_confirms: maker.config.required_confirms,
                    minimum_locktime: maker.config.min_contract_reaction_time,
                    max_size,
                    min_size: maker.config.min_size,
                    tweakable_point,
                }))
            }
            TakerToMakerMessage::ReqContractSigsForSender(message) => {
                connection_state.allowed_message = ExpectedMessage::ProofOfFunding;
                Some(maker.handle_req_contract_sigs_for_sender(message)?)
            }
            TakerToMakerMessage::RespProofOfFunding(proof) => {
                connection_state.allowed_message =
                    ExpectedMessage::ProofOfFundingORContractSigsForRecvrAndSender;
                Some(maker.handle_proof_of_funding(connection_state, proof, ip)?)
            }
            TakerToMakerMessage::ReqContractSigsForRecvr(message) => {
                connection_state.allowed_message = ExpectedMessage::HashPreimage;
                Some(maker.handle_sign_receivers_contract_tx(message)?)
            }
            TakerToMakerMessage::RespHashPreimage(message) => {
                connection_state.allowed_message = ExpectedMessage::PrivateKeyHandover;
                Some(maker.handle_hash_preimage(message)?)
            }
            _ => {
                log::info!("Newlyconnected taker stage message: {:?} ", message);
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
                    got: format!("{}", message),
                });
            }
        }
        ExpectedMessage::ProofOfFunding => {
            if let TakerToMakerMessage::RespProofOfFunding(proof) = message {
                connection_state.allowed_message =
                    ExpectedMessage::ProofOfFundingORContractSigsForRecvrAndSender;
                Some(maker.handle_proof_of_funding(connection_state, proof, ip)?)
            } else {
                return Err(MakerError::UnexpectedMessage {
                    expected: "Proof OF Funding".to_string(),
                    got: format!("{}", message),
                });
            }
        }
        ExpectedMessage::ProofOfFundingORContractSigsForRecvrAndSender => {
            match message {
                TakerToMakerMessage::RespProofOfFunding(proof) => {
                    connection_state.allowed_message =
                        ExpectedMessage::ProofOfFundingORContractSigsForRecvrAndSender;
                    Some(maker.handle_proof_of_funding(connection_state, proof, ip)?)
                }
                TakerToMakerMessage::RespContractSigsForRecvrAndSender(message) => {
                    // Nothing to send. Maker now creates and broadcasts his funding Txs
                    connection_state.allowed_message = ExpectedMessage::ReqContractSigsForRecvr;
                    maker
                        .handle_senders_and_receivers_contract_sigs(connection_state, message, ip)
                        .await?;
                    None
                }
                _ => {
                    return Err(MakerError::General(
                        "Expected proof of funding or sender's and reciever's contract signatures",
                    ));
                }
            }
        }
        ExpectedMessage::ReqContractSigsForRecvr => {
            if let TakerToMakerMessage::ReqContractSigsForRecvr(message) = message {
                connection_state.allowed_message = ExpectedMessage::HashPreimage;
                Some(maker.handle_sign_receivers_contract_tx(message)?)
            } else {
                return Err(MakerError::General(
                    "Expected reciever's contract transaction",
                ));
            }
        }
        ExpectedMessage::HashPreimage => {
            if let TakerToMakerMessage::RespHashPreimage(message) = message {
                connection_state.allowed_message = ExpectedMessage::PrivateKeyHandover;
                Some(maker.handle_hash_preimage(message)?)
            } else {
                return Err(MakerError::General("Expected hash preimgae"));
            }
        }
        ExpectedMessage::PrivateKeyHandover => {
            if let TakerToMakerMessage::RespPrivKeyHandover(message) = message {
                // Nothing to send. Succesfully completed swap
                maker.handle_private_key_handover(message)?;
                None
            } else {
                return Err(MakerError::General("expected privatekey handover"));
            }
        }
    };

    Ok(outgoing_message)
}

impl Maker {
    /// This is the first message handler for the Maker. It receives a [ReqContractSigsForSender] message,
    /// checks the validity of contract transactions, and provide's the signature for the sender side.
    /// This will fail if the maker doesn't have enough utxos to fund the next coinswap hop, or the contract
    /// transaction isn't valid.
    pub fn handle_req_contract_sigs_for_sender(
        &self,
        message: ReqContractSigsForSender,
    ) -> Result<MakerToTakerMessage, MakerError> {
        if let MakerBehavior::CloseBeforeSendingSendersSigs = self.behavior {
            return Err(MakerError::General(
                "closing connection early due to special maker behavior",
            ));
        }

        // Verify and sign the contract transaction, check function definition for all the checks.
        let sigs = self.verify_and_sign_contract_tx(&message)?;

        let funding_txids = message
            .txs_info
            .iter()
            .map(|txinfo| txinfo.senders_contract_tx.input[0].previous_output.txid)
            .collect::<Vec<_>>();

        let total_funding_amount = message
            .txs_info
            .iter()
            .fold(0u64, |acc, txinfo| acc + txinfo.funding_input_value);

        if total_funding_amount >= self.config.min_size
            && total_funding_amount < self.wallet.read()?.store.offer_maxsize
        {
            log::info!(
                "messageed contracts amount={}, for funding txids = {:?}",
                Amount::from_sat(total_funding_amount),
                funding_txids
            );
            Ok(MakerToTakerMessage::RespContractSigsForSender(
                ContractSigsForSender { sigs },
            ))
        } else {
            log::info!(
                "rejecting contracts for amount={} because not enough funds",
                Amount::from_sat(total_funding_amount)
            );
            Err(MakerError::General("not enough funds"))
        }
    }

    /// Validates the [ProofOfFunding] message, initiate the next hop,
    /// and create the [ReqContractSigsAsRecvrAndSender] message.
    pub fn handle_proof_of_funding(
        &self,
        connection_state: &mut ConnectionState,
        message: ProofOfFunding,
        ip: IpAddr,
    ) -> Result<MakerToTakerMessage, MakerError> {
        if let MakerBehavior::CloseAfterSendingSendersSigs = self.behavior {
            return Err(MakerError::General(
                "Special Behavior: Closing connection after sending sender's signatures",
            ));
        }

        // Basic verification of ProofOfFunding Message.
        // Check function definition for all the checks performed.
        let hashvalue = self.verify_proof_of_funding(&message)?;
        log::debug!("proof of funding valid, creating own funding txes");

        // Import transactions and addresses into Bitcoin core's wallet.
        // Add IncomingSwapcoin to Maker's Wallet
        for funding_info in &message.confirmed_funding_txes {
            let (pubkey1, pubkey2) =
                read_pubkeys_from_multisig_redeemscript(&funding_info.multisig_redeemscript)?;

            let funding_output_index = find_funding_output_index(&funding_info)?;
            let funding_output = funding_info
                .funding_tx
                .output
                .iter()
                .nth(funding_output_index as usize)
                .expect("funding output expected at this index");

            self.wallet
                .read()?
                .import_wallet_multisig_redeemscript(&pubkey1, &pubkey2)?;
            self.wallet.read()?.import_tx_with_merkleproof(
                &funding_info.funding_tx,
                &funding_info.funding_tx_merkleproof,
            )?;
            self.wallet
                .read()?
                .import_wallet_contract_redeemscript(&funding_info.contract_redeemscript)?;

            let receiver_contract_tx = create_receivers_contract_tx(
                OutPoint {
                    txid: funding_info.funding_tx.txid(),
                    vout: funding_output_index as u32,
                },
                funding_output.value,
                &funding_info.contract_redeemscript,
            );

            let (tweakable_privkey, _) = self.wallet.read()?.get_tweakable_keypair();
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

            log::debug!(
                "Adding incoming_swapcoin contract_tx = {:?} fo = {:?}",
                receiver_contract_tx.clone(),
                funding_output
            );

            connection_state
                .incoming_swapcoins
                .push(IncomingSwapCoin::new(
                    multisig_privkey,
                    other_pubkey,
                    receiver_contract_tx.clone(),
                    funding_info.contract_redeemscript.clone(),
                    hashlock_privkey,
                    funding_output.value,
                ));
        }

        // Calculate output amounts for the next hop
        let incoming_amount = message.confirmed_funding_txes.iter().fold(0u64, |acc, fi| {
            let index = find_funding_output_index(fi).unwrap();
            let txout = fi
                .funding_tx
                .output
                .iter()
                .nth(index as usize)
                .expect("output at index expected");
            acc + txout.value
        });

        let coinswap_fees = calculate_coinswap_fee(
            self.config.absolute_fee_sats,
            self.config.amount_relative_fee_ppb,
            self.config.time_relative_fee_ppb,
            incoming_amount,
            self.config.required_confirms, //time_in_blocks just 1 for now
        );
        let miner_fees_paid_by_taker = FUNDING_TX_VBYTE_SIZE
            * message.next_fee_rate
            * (message.next_coinswap_info.len() as u64)
            / 1000;

        let outgoing_amount = incoming_amount - coinswap_fees - miner_fees_paid_by_taker;

        // Create outgoing coinswap of the next hop
        let (my_funding_txes, outgoing_swapcoins, total_miner_fee) = {
            self.wallet.write()?.initalize_coinswap(
                outgoing_amount,
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
                message.next_locktime,
                message.next_fee_rate,
            )?
        };

        log::info!(
            "Proof of funding valid. Incoming funding txes, txids = {:?}",
            message
                .confirmed_funding_txes
                .iter()
                .map(|cft| cft.funding_tx.txid())
                .collect::<Vec<Txid>>()
        );
        log::info!(
            "incoming_amount={}, incoming_locktime={}, hashvalue={}",
            Amount::from_sat(incoming_amount),
            read_contract_locktime(&message.confirmed_funding_txes[0].contract_redeemscript)
                .unwrap(),
            //unwrap() as format of contract_redeemscript already checked in verify_proof_of_funding
            hashvalue
        );
        log::info!(
            concat!(
                "outgoing_amount={}, outgoing_locktime={}, miner fees paid by taker={}, ",
                "actual miner fee={}, coinswap_fees={}, POTENTIALLY EARNED={}"
            ),
            Amount::from_sat(outgoing_amount),
            message.next_locktime,
            Amount::from_sat(miner_fees_paid_by_taker),
            Amount::from_sat(total_miner_fee),
            Amount::from_sat(coinswap_fees),
            Amount::from_sat(incoming_amount - outgoing_amount - total_miner_fee)
        );

        connection_state.pending_funding_txes = my_funding_txes;
        connection_state.outgoing_swapcoins = outgoing_swapcoins;
        log::debug!(
            "Incoming_swapcoins = {:#?}\nOutgoing_swapcoins = {:#?}",
            connection_state.incoming_swapcoins,
            connection_state.outgoing_swapcoins,
        );

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
            .map(|outgoing_swapcoin| SenderContractTxInfo {
                contract_tx: outgoing_swapcoin.contract_tx.clone(),
                timelock_pubkey: outgoing_swapcoin.get_timelock_pubkey(),
                multisig_redeemscript: outgoing_swapcoin.get_multisig_redeemscript(),
                funding_amount: outgoing_swapcoin.funding_amount,
            })
            .collect::<Vec<SenderContractTxInfo>>();

        // Update the connection state.
        self.connection_state
            .write()
            .unwrap()
            .insert(ip, (connection_state.clone(), Instant::now()));

        Ok(MakerToTakerMessage::ReqContractSigsAsRecvrAndSender(
            ContractSigsAsRecvrAndSender {
                receivers_contract_txs,
                senders_contract_txs_info,
            },
        ))
    }

    /// Handles [ContractSigsForRecvrAndSender] message and updates the wallet state
    pub async fn handle_senders_and_receivers_contract_sigs(
        &self,
        connection_state: &mut ConnectionState,
        message: ContractSigsForRecvrAndSender,
        ip: IpAddr,
    ) -> Result<(), MakerError> {
        if message.receivers_sigs.len() != connection_state.incoming_swapcoins.len() {
            return Err(MakerError::General(
                "invalid number of reciever's signatures",
            ));
        }
        for (receivers_sig, incoming_swapcoin) in message
            .receivers_sigs
            .iter()
            .zip(connection_state.incoming_swapcoins.iter_mut())
        {
            incoming_swapcoin.verify_contract_tx_sig(receivers_sig)?;
            incoming_swapcoin.others_contract_sig = Some(receivers_sig.clone());
        }

        if message.senders_sigs.len() != connection_state.outgoing_swapcoins.len() {
            return Err(MakerError::General("invalid number of sender's signatures"));
        }

        for (senders_sig, outgoing_swapcoin) in message
            .senders_sigs
            .iter()
            .zip(connection_state.outgoing_swapcoins.iter_mut())
        {
            outgoing_swapcoin.verify_contract_tx_sig(senders_sig)?;

            outgoing_swapcoin.others_contract_sig = Some(senders_sig.clone());
        }

        let mut my_funding_txids = Vec::<Txid>::new();
        for my_funding_tx in &connection_state.pending_funding_txes {
            log::debug!("Broadcasting My Funding Tx : {:#?}", my_funding_tx);
            let txid = self
                .wallet
                .read()?
                .rpc
                .send_raw_transaction(my_funding_tx)
                .map_err(|e| MakerError::Wallet(e.into()))?;
            assert_eq!(txid, my_funding_tx.txid());
            my_funding_txids.push(txid);
        }
        log::info!("Broadcasted My Funding Txes: {:?}", my_funding_txids);

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

        // Update the connection state.
        self.connection_state
            .write()
            .unwrap()
            .insert(ip, (connection_state.clone(), Instant::now()));

        Ok(())
    }

    /// Handles [ReqContractSigsForRecvr] and returns a [MakerToTakerMessage::RespContractSigsForRecvr]
    pub fn handle_sign_receivers_contract_tx(
        &self,
        message: ReqContractSigsForRecvr,
    ) -> Result<MakerToTakerMessage, MakerError> {
        let mut sigs = Vec::<Signature>::new();
        for receivers_contract_tx_info in &message.txs {
            sigs.push(
                //the fact that the peer knows the correct multisig_redeemscript is what ensures
                //security here, a random peer out there who isnt involved in a coinswap wont know
                //what the multisig_redeemscript is
                self.wallet
                    .read()?
                    .find_outgoing_swapcoin(&receivers_contract_tx_info.multisig_redeemscript)
                    .expect("Outgoing swapcoin not found")
                    .sign_contract_tx_with_my_privkey(&receivers_contract_tx_info.contract_tx)?,
            );
        }
        Ok(MakerToTakerMessage::RespContractSigsForRecvr(
            ContractSigsForRecvr { sigs },
        ))
    }

    /// Handles a [HashPreimage] message and returns a [MakerToTakerMessage::RespPrivKeyHandover]
    pub fn handle_hash_preimage(
        &self,
        message: HashPreimage,
    ) -> Result<MakerToTakerMessage, MakerError> {
        let hashvalue = Hash160::hash(&message.preimage);
        for multisig_redeemscript in &message.senders_multisig_redeemscripts {
            let mut wallet_write = self.wallet.write()?;
            let incoming_swapcoin = wallet_write
                .find_incoming_swapcoin_mut(&multisig_redeemscript)
                .expect("Incoming swampcoin expected");
            if read_hashvalue_from_contract(&incoming_swapcoin.contract_redeemscript)? != hashvalue
            {
                return Err(MakerError::General("not correct hash preimage"));
            }
            incoming_swapcoin.hash_preimage = Some(message.preimage);
        }
        //TODO tell preimage to watchtowers

        log::info!("received preimage for hashvalue={}", hashvalue);
        let mut swapcoin_private_keys = Vec::<MultisigPrivkey>::new();

        for multisig_redeemscript in &message.receivers_multisig_redeemscripts {
            let wallet_read = self.wallet.read()?;
            let outgoing_swapcoin = wallet_read
                .find_outgoing_swapcoin(&multisig_redeemscript)
                .expect("outgoing swapcoin expected");
            if read_hashvalue_from_contract(&outgoing_swapcoin.contract_redeemscript)? != hashvalue
            {
                return Err(MakerError::General("not correct hash preimage"));
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
    pub fn handle_private_key_handover(&self, message: PrivKeyHandover) -> Result<(), MakerError> {
        for swapcoin_private_key in &message.multisig_privkeys {
            self.wallet
                .write()?
                .find_incoming_swapcoin_mut(&swapcoin_private_key.multisig_redeemscript)
                .expect("incoming swapcoin not found")
                .apply_privkey(swapcoin_private_key.key)?
        }
        self.wallet.write()?.save_to_disk()?;
        log::info!("Successfully Completed Coinswap");
        Ok(())
    }
}

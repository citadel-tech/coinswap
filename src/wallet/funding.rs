#![allow(dead_code)]
//! Various mechanisms of creating the swap funding transactions.
//!
//! This module contains routines for creating funding transactions within a wallet. It leverages
//! Bitcoin Core's RPC methods for wallet interactions, including `walletcreatefundedpsbt`

use std::{collections::HashMap, iter};

use bitcoin::{
    absolute::LockTime, transaction::Version, Address, Amount, OutPoint, ScriptBuf, Sequence,
    Transaction, TxIn, TxOut, Txid, Witness,
};

use bitcoind::bitcoincore_rpc::{json::CreateRawTransactionInput, RpcApi};

use bitcoin::secp256k1::rand::{rngs::OsRng, RngCore};

use crate::{taker::api::MINER_FEE, wallet::Destination};

use super::Wallet;

use super::error::WalletError;

#[derive(Debug)]
pub(crate) struct CreateFundingTxesResult {
    pub(crate) funding_txes: Vec<Transaction>,
    pub(crate) payment_output_positions: Vec<u32>,
    pub(crate) total_miner_fee: u64,
}

impl Wallet {
    // Attempts to create the funding transactions.
    /// Returns Ok(None) if there was no error but the wallet was unable to create funding txes
    pub(crate) fn create_funding_txes(
        &mut self,
        coinswap_amount: Amount,
        destinations: &[Address],
        fee_rate: Amount,
    ) -> Result<CreateFundingTxesResult, WalletError> {
        let ret = self.create_funding_txes_random_amounts(coinswap_amount, destinations, fee_rate);
        if ret.is_ok() {
            log::info!(target: "wallet", "created funding txes with random amounts");
            return ret;
        }

        // TODO: Unlock this code when we are sure that the routines actually works.

        // let ret = self.create_funding_txes_utxo_max_sends(coinswap_amount, destinations, fee_rate);
        // if ret.is_ok() {
        //     log::info!(target: "wallet", "created funding txes with fully-spending utxos");
        //     return ret;
        // }

        // let ret =
        //     self.create_funding_txes_use_biggest_utxos(coinswap_amount, destinations, fee_rate);
        // if ret.is_ok() {
        //     log::info!(target: "wallet", "created funding txes with using the biggest utxos");
        //     return ret;
        // }

        log::info!("failed to create funding txes with any method {ret:?}");
        ret
    }

    fn generate_amount_fractions_without_correction(
        count: usize,
        total_amount: Amount,
        lower_limit: u64,
    ) -> Result<Vec<f32>, WalletError> {
        for _ in 0..100000 {
            let mut knives = (1..count)
                .map(|_| (OsRng.next_u32() as f32) / (u32::MAX as f32))
                .collect::<Vec<f32>>();
            knives.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));

            let mut fractions = Vec::<f32>::new();
            let mut last: f32 = 1.0;
            for k in knives {
                fractions.push(last - k);
                last = k;
            }
            fractions.push(last);

            if fractions
                .iter()
                .all(|f| *f * (total_amount.to_sat() as f32) > (lower_limit as f32))
            {
                return Ok(fractions);
            }
        }
        Err(WalletError::General(
            "Unable to generate amount fractions, probably amount too small".to_string(),
        ))
    }

    pub(crate) fn generate_amount_fractions(
        count: usize,
        total_amount: Amount,
    ) -> Result<Vec<u64>, WalletError> {
        let mut output_values = Wallet::generate_amount_fractions_without_correction(
            count,
            total_amount,
            5000, //use 5000 satoshi as the lower limit for now
                  //there should always be enough to pay miner fees
        )?
        .iter()
        .map(|f| (*f * (total_amount.to_sat() as f32)) as u64)
        .collect::<Vec<u64>>();

        //rounding errors mean usually 1 or 2 satoshis are lost, add them back

        //this calculation works like this:
        //o = [a, b, c, ...]             | list of output values
        //t = coinswap amount            | total desired value
        //a' <-- a + (t - (a+b+c+...))   | assign new first output value
        //a' <-- a + (t -a-b-c-...)      | rearrange
        //a' <-- t - b - c -...          |
        *output_values.first_mut().expect("value expected") =
            total_amount.to_sat() - output_values.iter().skip(1).sum::<u64>();
        assert_eq!(output_values.iter().sum::<u64>(), total_amount.to_sat());

        Ok(output_values)
    }

    /// This function creates funding transactions with random amounts
    /// The total `coinswap_amount` is randomly distributed among number of destinations.
    fn create_funding_txes_random_amounts(
        &mut self,
        coinswap_amount: Amount,
        destinations: &[Address],
        fee_rate: Amount,
    ) -> Result<CreateFundingTxesResult, WalletError> {
        let output_values = Wallet::generate_amount_fractions(destinations.len(), coinswap_amount)?;

        // Flow of Lock Step 1. Unlock all unspent UTXOs
        self.rpc.unlock_unspent_all()?;

        // FLow of Lock Step 2. Lock all unspendable UTXOs
        self.lock_unspendable_utxos()?;

        let mut funding_txes = Vec::<Transaction>::new();
        let mut payment_output_positions = Vec::<u32>::new();
        let mut total_miner_fee = 0;
        let mut locked_utxos = Vec::new();

        // Here, we are gonna use a closure to ensure proper cleanup on error (since we need a rollback)
        let result = (|| {
            for (address, &output_value) in destinations.iter().zip(output_values.iter()) {
                let remaining = Amount::from_sat(output_value);
                let selected_utxo = self.coin_select(remaining, fee_rate.to_btc())?;

                let outpoints: Vec<OutPoint> = selected_utxo
                    .iter()
                    .map(|(utxo, _)| OutPoint::new(utxo.txid, utxo.vout))
                    .collect();
                // Flow of Lock Step 3. Lock the selected UTXOs immediately after selection
                self.rpc.lock_unspent(&outpoints)?;
                // Flow of Lock Step 4. Store the locked UTXOs for later unlocking in case of error
                locked_utxos.extend(outpoints);

                let total_input_amount =
                    selected_utxo
                        .iter()
                        .fold(Amount::ZERO, |acc, (unspent, _)| {
                            acc.checked_add(unspent.amount)
                                .expect("Amount sum overflowed")
                        });

                // Here, prepare coins for spend_coins API, since this API would require owned data to avoid lifetime issues
                let coins_to_spend = selected_utxo
                    .iter()
                    .map(|(unspent, spend_info)| (unspent.clone(), spend_info.clone()))
                    .collect::<Vec<_>>();

                // Create destination with output - currently, destination is an array with a single address, i.e only a single transaction.
                let destination =
                    Destination::Multi(vec![(address.clone(), Amount::from_sat(output_value))]);

                // Creates and Signs Transactions via the spend_coins API
                let funding_tx =
                    self.spend_coins(&coins_to_spend, destination, fee_rate.to_sat() as f64)?;

                // The actual fee is the difference between the sum of output amounts from the total input amount
                let actual_fee = total_input_amount
                    - (funding_tx.output.iter().fold(Amount::ZERO, |a, txo| {
                        a.checked_add(txo.value)
                            .expect("output amount summation overflowed")
                    }));

                let tx_size = funding_tx.weight().to_vbytes_ceil();
                // Note : The feerates are sats/vbyte
                let actual_feerate = actual_fee.to_sat() as f32 / tx_size as f32;

                log::info!(
                    "Created Funding tx, txid: {} | Size: {} vB | Fee: {} sats | Feerate: {:.2} sat/vB",
                    funding_tx.compute_txid(),
                    tx_size,
                    actual_fee.to_sat(),
                    actual_feerate
                );

                // Record this transaction in our results.
                let payment_pos = 0; // assuming the payment output position is 0

                funding_txes.push(funding_tx);
                payment_output_positions.push(payment_pos);
                total_miner_fee += fee_rate.to_sat();
            }
            Ok(CreateFundingTxesResult {
                funding_txes,
                payment_output_positions,
                total_miner_fee,
            })
        })();

        // FLow of Lock Step 5. We unlock the UTXOs on error i.e a rollback mechanism, OR keep locked on success
        if result.is_err() {
            self.rpc.unlock_unspent(&locked_utxos)?;
        }

        result
    }

    fn create_mostly_sweep_txes_with_one_tx_having_change(
        &self,
        coinswap_amount: Amount,
        destinations: &[Address],
        fee_rate: Amount,
        change_address: &Address,
        utxos: &mut dyn Iterator<Item = (Txid, u32, u64)>, //utxos item is (txid, vout, value)
                                                           //utxos should be sorted by size, largest first
    ) -> Result<CreateFundingTxesResult, WalletError> {
        let mut funding_txes = Vec::<Transaction>::new();
        let mut payment_output_positions = Vec::<u32>::new();
        let mut total_miner_fee = 0;

        let mut leftover_coinswap_amount = coinswap_amount;
        let mut destinations_iter = destinations.iter();
        let first_tx_input = utxos.next().unwrap();

        // Set the Anti-Fee-Snipping locktime
        let current_height = self.rpc.get_block_count()?;
        let lock_time = LockTime::from_height(current_height as u32)?;

        for _ in 0..destinations.len() - 2 {
            let (txid, vout, value) = utxos.next().unwrap();

            let mut outputs = HashMap::<&Address, u64>::new();
            outputs.insert(destinations_iter.next().unwrap(), value);
            let tx_inputs = vec![TxIn {
                previous_output: OutPoint::new(txid, vout),
                sequence: Sequence(0),
                witness: Witness::new(),
                script_sig: ScriptBuf::new(),
            }];
            let mut input_info = iter::once(self.get_utxo((txid, vout))?.unwrap());

            let mut tx_outs = Vec::new();
            for (address, value) in outputs {
                tx_outs.push(TxOut {
                    value: Amount::from_sat(value),
                    script_pubkey: address.script_pubkey(),
                });
            }

            let mut funding_tx = Transaction {
                input: tx_inputs,
                output: tx_outs,
                lock_time,
                version: Version::TWO,
            };
            self.sign_transaction(&mut funding_tx, &mut input_info)?;

            leftover_coinswap_amount -= funding_tx.output[0].value;

            total_miner_fee += fee_rate.to_sat();

            funding_txes.push(funding_tx);
            payment_output_positions.push(0);
        }
        let mut tx_inputs = Vec::new();
        let mut input_info = Vec::new();
        let (_leftover_inputs, leftover_inputs_values): (Vec<_>, Vec<_>) = utxos
            .map(|(txid, vout, value)| {
                tx_inputs.push(TxIn {
                    previous_output: OutPoint::new(txid, vout),
                    sequence: Sequence(0),
                    witness: Witness::new(),
                    script_sig: ScriptBuf::new(),
                });
                input_info.push(self.get_utxo((txid, vout)).unwrap().unwrap());
                (
                    CreateRawTransactionInput {
                        txid,
                        vout,
                        sequence: None,
                    },
                    value,
                )
            })
            .unzip();
        let mut outputs = HashMap::<&Address, u64>::new();
        outputs.insert(
            destinations_iter.next().unwrap(),
            leftover_inputs_values.iter().sum::<u64>(),
        );
        let mut tx_outs = Vec::new();
        for (address, value) in outputs {
            tx_outs.push(TxOut {
                value: Amount::from_sat(value),
                script_pubkey: address.script_pubkey(),
            });
        }

        let mut funding_tx = Transaction {
            input: tx_inputs,
            output: tx_outs,
            lock_time,
            version: Version::TWO,
        };
        let mut info = input_info.iter().cloned();
        self.sign_transaction(&mut funding_tx, &mut info)?;

        leftover_coinswap_amount -= funding_tx.output[0].value;

        total_miner_fee += fee_rate.to_sat();

        funding_txes.push(funding_tx);
        payment_output_positions.push(0);

        let (first_txid, first_vout, first_value) = first_tx_input;
        let mut outputs = HashMap::<&Address, u64>::new();
        outputs.insert(
            destinations_iter.next().unwrap(),
            leftover_coinswap_amount.to_sat(),
        );

        tx_inputs = Vec::new();
        tx_outs = Vec::new();
        let mut change_amount = first_value;
        tx_inputs.push(TxIn {
            previous_output: OutPoint::new(first_txid, first_vout),
            sequence: Sequence(0),
            witness: Witness::new(),
            script_sig: ScriptBuf::new(),
        });
        for (address, value) in outputs {
            change_amount -= value;
            tx_outs.push(TxOut {
                value: Amount::from_sat(value),
                script_pubkey: address.script_pubkey(),
            });
        }
        tx_outs.push(TxOut {
            value: Amount::from_sat(change_amount),
            script_pubkey: change_address.script_pubkey(),
        });
        let mut funding_tx = Transaction {
            input: tx_inputs,
            output: tx_outs,
            lock_time,
            version: Version::TWO,
        };
        let mut info = iter::once(self.get_utxo((first_txid, first_vout))?.unwrap());
        self.sign_transaction(&mut funding_tx, &mut info)?;

        total_miner_fee += fee_rate.to_sat();

        funding_txes.push(funding_tx);
        payment_output_positions.push(1);

        Ok(CreateFundingTxesResult {
            funding_txes,
            payment_output_positions,
            total_miner_fee,
        })
    }

    fn create_funding_txes_utxo_max_sends(
        &mut self,
        coinswap_amount: Amount,
        destinations: &[Address],
        fee_rate: Amount,
    ) -> Result<CreateFundingTxesResult, WalletError> {
        //this function creates funding txes by
        //using walletcreatefundedpsbt for the total amount, and if
        //the number if inputs UTXOs is >number_of_txes then split those inputs into groups
        //across multiple transactions

        let mut outputs = HashMap::<&Address, u64>::new();
        outputs.insert(&destinations[0], coinswap_amount.to_sat());
        let change_address = self.get_next_internal_addresses(1)?[0].clone();

        self.lock_unspendable_utxos()?;

        let fee = Amount::from_sat(MINER_FEE);

        let remaining = coinswap_amount;

        let selected_utxo = self.coin_select(remaining + fee, fee_rate.to_btc())?;

        let total_input_amount = selected_utxo.iter().fold(Amount::ZERO, |acc, (unspet, _)| {
            acc.checked_add(unspet.amount)
                .expect("Amount sum overflowed")
        });

        let change_amount = total_input_amount.checked_sub(remaining + fee);

        let mut tx_outs = vec![TxOut {
            value: coinswap_amount,
            script_pubkey: destinations[0].script_pubkey(),
        }];

        if let Some(change) = change_amount {
            tx_outs.push(TxOut {
                value: change,
                script_pubkey: change_address.script_pubkey(),
            });
        }

        let tx_inputs = selected_utxo
            .iter()
            .map(|(unspent, _)| TxIn {
                previous_output: OutPoint::new(unspent.txid, unspent.vout),
                sequence: Sequence(0),
                witness: Witness::new(),
                script_sig: ScriptBuf::new(),
            })
            .collect::<Vec<_>>();

        // Set the Anti-Fee-Snipping locktime
        let current_height = self.rpc.get_block_count()?;
        let lock_time = LockTime::from_height(current_height as u32)?;

        let mut funding_tx = Transaction {
            input: tx_inputs,
            output: tx_outs,
            lock_time,
            version: Version::TWO,
        };

        let mut input_info = selected_utxo
            .iter()
            .map(|(_, spend_info)| spend_info.clone());
        self.sign_transaction(&mut funding_tx, &mut input_info)?;

        let total_tx_inputs_len = selected_utxo.len();
        if total_tx_inputs_len < destinations.len() {
            return Err(WalletError::General(
                "Not enough UTXOs found, can't use this method".to_string(),
            ));
        }

        self.create_mostly_sweep_txes_with_one_tx_having_change(
            coinswap_amount,
            destinations,
            fee_rate,
            &change_address,
            &mut selected_utxo
                .iter()
                .map(|(l, _)| (l.txid, l.vout, l.amount.to_sat())),
        )
    }

    fn create_funding_txes_use_biggest_utxos(
        &self,
        coinswap_amount: Amount,
        destinations: &[Address],
        fee_rate: Amount,
    ) -> Result<CreateFundingTxesResult, WalletError> {
        //this function will pick the top most valuable UTXOs and use them
        //to create funding transactions

        let mut seed_coin_utxo = self.list_descriptor_utxo_spend_info()?;
        let mut swap_coin_utxo = self.list_swap_coin_utxo_spend_info()?;
        seed_coin_utxo.append(&mut swap_coin_utxo);

        let mut list_unspent_result = seed_coin_utxo;
        if list_unspent_result.len() < destinations.len() {
            return Err(WalletError::General(
                "Not enough UTXOs to create this many funding txes".to_string(),
            ));
        }
        list_unspent_result.sort_by(|(a, _), (b, _)| {
            b.amount
                .to_sat()
                .partial_cmp(&a.amount.to_sat())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut list_unspent_count: Option<usize> = None;
        for ii in destinations.len()..list_unspent_result.len() + 1 {
            let sum = list_unspent_result[..ii]
                .iter()
                .map(|(l, _)| l.amount.to_sat())
                .sum::<u64>();
            if sum > coinswap_amount.to_sat() {
                list_unspent_count = Some(ii);
                break;
            }
        }
        if list_unspent_count.is_none() {
            return Err(WalletError::General(
                "Not enough UTXOs/value to create funding txes".to_string(),
            ));
        }

        let inputs = &list_unspent_result[..list_unspent_count.unwrap()];

        if inputs[1..]
            .iter()
            .map(|(l, _)| l.amount.to_sat())
            .any(|utxo_value| utxo_value > coinswap_amount.to_sat())
        {
            // TODO: Handle this case
            Err(WalletError::General(
                "Some stupid error that will never occur".to_string(),
            ))
        } else {
            //at most one utxo bigger than the coinswap amount

            let change_address = &self.get_next_internal_addresses(1)?[0];
            self.create_mostly_sweep_txes_with_one_tx_having_change(
                coinswap_amount,
                destinations,
                fee_rate,
                change_address,
                &mut inputs.iter().map(|(list_unspent_entry, _spend_info)| {
                    (
                        list_unspent_entry.txid,
                        list_unspent_entry.vout,
                        list_unspent_entry.amount.to_sat(),
                    )
                }),
            )
        }
    }
}

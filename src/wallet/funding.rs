//! Various mechanisms of creating the swap funding transactions.
//!
//! This module contains routines for creating funding transactions within a wallet. It leverages
//! Bitcoin Core's RPC methods for wallet interactions, including `walletcreatefundedpsbt`

use std::collections::HashMap;

use bitcoin::{address::NetworkUnchecked, Address, Amount, OutPoint, Transaction, Txid};

use bitcoind::bitcoincore_rpc::{
    json::{CreateRawTransactionInput, WalletCreateFundedPsbtOptions},
    RpcApi,
};

use serde_json::Value;

use bitcoin::secp256k1::rand::{rngs::OsRng, RngCore};

use crate::utill::convert_json_rpc_bitcoin_to_satoshis;

use super::Wallet;

use super::error::WalletError;

pub struct CreateFundingTxesResult {
    pub funding_txes: Vec<Transaction>,
    pub payment_output_positions: Vec<u32>,
    pub total_miner_fee: u64,
}

impl Wallet {
    pub fn create_funding_txes(
        &self,
        coinswap_amount: u64,
        destinations: &[Address],
        fee_rate: u64,
    ) -> Result<Option<CreateFundingTxesResult>, WalletError> {
        //returns Ok(None) if there was no error but the wallet was unable to create funding txes

        log::debug!(target: "wallet", "coinswap_amount = {} destinations = {:?}",
            coinswap_amount, destinations);

        let ret = self.create_funding_txes_random_amounts(coinswap_amount, destinations, fee_rate);
        if ret.is_ok() {
            log::debug!(target: "wallet", "created funding txes with random amounts");
            return ret;
        }

        let ret = self.create_funding_txes_utxo_max_sends(coinswap_amount, destinations, fee_rate);
        if ret.is_ok() {
            log::debug!(target: "wallet", "created funding txes with fully-spending utxos");
            return ret;
        }

        let ret =
            self.create_funding_txes_use_biggest_utxos(coinswap_amount, destinations, fee_rate);
        if ret.is_ok() {
            log::debug!(target: "wallet", "created funding txes with using the biggest utxos");
            return ret;
        }

        log::debug!(target: "wallet", "failed to create funding txes with any method");
        ret
    }

    fn generate_amount_fractions_without_correction(
        count: usize,
        total_amount: u64,
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
                .all(|f| *f * (total_amount as f32) > (lower_limit as f32))
            {
                return Ok(fractions);
            }
        }
        Err(WalletError::Protocol(
            "Unable to generate amount fractions, probably amount too small".to_string(),
        ))
    }

    fn generate_amount_fractions(count: usize, total_amount: u64) -> Result<Vec<u64>, WalletError> {
        let mut output_values = Wallet::generate_amount_fractions_without_correction(
            count,
            total_amount,
            5000, //use 5000 satoshi as the lower limit for now
                  //there should always be enough to pay miner fees
        )?
        .iter()
        .map(|f| (*f * (total_amount as f32)) as u64)
        .collect::<Vec<u64>>();

        //rounding errors mean usually 1 or 2 satoshis are lost, add them back

        //this calculation works like this:
        //o = [a, b, c, ...]             | list of output values
        //t = coinswap amount            | total desired value
        //a' <-- a + (t - (a+b+c+...))   | assign new first output value
        //a' <-- a + (t -a-b-c-...)      | rearrange
        //a' <-- t - b - c -...          |
        *output_values.first_mut().unwrap() =
            total_amount - output_values.iter().skip(1).sum::<u64>();
        assert_eq!(output_values.iter().sum::<u64>(), total_amount);
        log::debug!(target: "wallet", "output values = {:?}", output_values);

        Ok(output_values)
    }

    fn create_funding_txes_random_amounts(
        &self,
        coinswap_amount: u64,
        destinations: &[Address],
        fee_rate: u64,
    ) -> Result<Option<CreateFundingTxesResult>, WalletError> {
        //this function creates funding txes by
        //randomly generating some satoshi amounts and send them into
        //walletcreatefundedpsbt to create txes that create change

        let change_addresses = self.get_next_internal_addresses(destinations.len() as u32)?;
        log::debug!(target: "wallet", "change addrs = {:?}", change_addresses);

        let output_values = Wallet::generate_amount_fractions(destinations.len(), coinswap_amount)?;

        self.lock_all_nonwallet_unspents()?;

        let mut funding_txes = Vec::<Transaction>::new();
        let mut payment_output_positions = Vec::<u32>::new();
        let mut total_miner_fee = 0;
        for ((address, &output_value), change_address) in destinations
            .iter()
            .zip(output_values.iter())
            .zip(change_addresses.iter())
        {
            log::debug!(target: "wallet", "output_value = {} to addr={}", output_value, address);

            let mut outputs = HashMap::<String, Amount>::new();
            outputs.insert(address.to_string(), Amount::from_sat(output_value));

            let change_addrs_unchecked: Address<NetworkUnchecked> =
                change_address.to_string().parse().unwrap();

            let wcfp_result = self.rpc.wallet_create_funded_psbt(
                &[],
                &outputs,
                None,
                Some(WalletCreateFundedPsbtOptions {
                    include_watching: Some(true),
                    change_address: Some(change_addrs_unchecked),
                    fee_rate: Some(Amount::from_sat(fee_rate)),
                    ..Default::default()
                }),
                None,
            )?;
            total_miner_fee += wcfp_result.fee.to_sat();
            log::debug!(target: "wallet", "created funding tx, miner fee={}", wcfp_result.fee);

            let funding_tx = self.from_walletcreatefundedpsbt_to_tx(&wcfp_result.psbt)?;

            self.rpc.lock_unspent(
                &funding_tx
                    .input
                    .iter()
                    .map(|vin| vin.previous_output)
                    .collect::<Vec<OutPoint>>(),
            )?;

            let payment_pos = if wcfp_result.change_position == 0 {
                1
            } else {
                0
            };
            log::debug!(target: "wallet", "payment_pos = {}", payment_pos);

            funding_txes.push(funding_tx);
            payment_output_positions.push(payment_pos);
        }

        Ok(Some(CreateFundingTxesResult {
            funding_txes,
            payment_output_positions,
            total_miner_fee,
        }))
    }

    fn create_mostly_sweep_txes_with_one_tx_having_change(
        &self,
        coinswap_amount: u64,
        destinations: &[Address],
        fee_rate: u64,
        change_address: &Address,
        utxos: &mut dyn Iterator<Item = (Txid, u32, u64)>, //utxos item is (txid, vout, value)
                                                           //utxos should be sorted by size, largest first
    ) -> Result<Option<CreateFundingTxesResult>, WalletError> {
        let mut funding_txes = Vec::<Transaction>::new();
        let mut payment_output_positions = Vec::<u32>::new();
        let mut total_miner_fee = 0;

        let mut leftover_coinswap_amount = coinswap_amount;
        let mut destinations_iter = destinations.iter();
        let first_tx_input = utxos.next().unwrap();

        for _ in 0..destinations.len() - 2 {
            let (txid, vout, value) = utxos.next().unwrap();

            let mut outputs = HashMap::<String, Amount>::new();
            outputs.insert(
                destinations_iter.next().unwrap().to_string(),
                Amount::from_sat(value),
            );
            let wcfp_result = self.rpc.wallet_create_funded_psbt(
                &[CreateRawTransactionInput {
                    txid,
                    vout,
                    sequence: None,
                }],
                &outputs,
                None,
                Some(WalletCreateFundedPsbtOptions {
                    add_inputs: Some(false),
                    subtract_fee_from_outputs: vec![0],
                    fee_rate: Some(Amount::from_sat(fee_rate)),
                    ..Default::default()
                }),
                None,
            )?;
            let funding_tx = self.from_walletcreatefundedpsbt_to_tx(&wcfp_result.psbt)?;
            leftover_coinswap_amount -= funding_tx.output[0].value;

            total_miner_fee += wcfp_result.fee.to_sat();
            log::debug!(target: "wallet", "created funding tx, miner fee={}", wcfp_result.fee);

            funding_txes.push(funding_tx);
            payment_output_positions.push(0);
        }

        let (leftover_inputs, leftover_inputs_values): (Vec<_>, Vec<_>) = utxos
            .map(|(txid, vout, value)| {
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
        let mut outputs = HashMap::<String, Amount>::new();
        outputs.insert(
            destinations_iter.next().unwrap().to_string(),
            Amount::from_sat(leftover_inputs_values.iter().sum::<u64>()),
        );
        let wcfp_result = self.rpc.wallet_create_funded_psbt(
            &leftover_inputs,
            &outputs,
            None,
            Some(WalletCreateFundedPsbtOptions {
                add_inputs: Some(false),
                subtract_fee_from_outputs: vec![0],
                fee_rate: Some(Amount::from_sat(fee_rate)),
                ..Default::default()
            }),
            None,
        )?;
        let funding_tx = self.from_walletcreatefundedpsbt_to_tx(&wcfp_result.psbt)?;
        leftover_coinswap_amount -= funding_tx.output[0].value;

        total_miner_fee += wcfp_result.fee.to_sat();
        log::debug!(target: "wallet", "created funding tx, miner fee={}", wcfp_result.fee);

        funding_txes.push(funding_tx);
        payment_output_positions.push(0);

        let (first_txid, first_vout, _first_value) = first_tx_input;
        let mut outputs = HashMap::<String, Amount>::new();
        outputs.insert(
            destinations_iter.next().unwrap().to_string(),
            Amount::from_sat(leftover_coinswap_amount),
        );
        let change_addrs_unchecked: Address<NetworkUnchecked> =
            change_address.to_string().parse().unwrap();

        let wcfp_result = self.rpc.wallet_create_funded_psbt(
            &[CreateRawTransactionInput {
                txid: first_txid,
                vout: first_vout,
                sequence: None,
            }],
            &outputs,
            None,
            Some(WalletCreateFundedPsbtOptions {
                add_inputs: Some(false),
                change_address: Some(change_addrs_unchecked),
                fee_rate: Some(Amount::from_sat(fee_rate)),
                ..Default::default()
            }),
            None,
        )?;
        let funding_tx = self.from_walletcreatefundedpsbt_to_tx(&wcfp_result.psbt)?;

        total_miner_fee += wcfp_result.fee.to_sat();
        log::debug!(target: "wallet", "created funding tx, miner fee={}", wcfp_result.fee);

        funding_txes.push(funding_tx);
        payment_output_positions.push(if wcfp_result.change_position == 0 {
            1
        } else {
            0
        });

        Ok(Some(CreateFundingTxesResult {
            funding_txes,
            payment_output_positions,
            total_miner_fee,
        }))
    }

    fn create_funding_txes_utxo_max_sends(
        &self,
        coinswap_amount: u64,
        destinations: &[Address],
        fee_rate: u64,
    ) -> Result<Option<CreateFundingTxesResult>, WalletError> {
        //this function creates funding txes by
        //using walletcreatefundedpsbt for the total amount, and if
        //the number if inputs UTXOs is >number_of_txes then split those inputs into groups
        //across multiple transactions

        let mut outputs = HashMap::<String, Amount>::new();
        outputs.insert(
            destinations[0].to_string(),
            Amount::from_sat(coinswap_amount),
        );
        let change_address = self.get_next_internal_addresses(1)?[0].clone();

        let change_addrs_uncheked: Address<NetworkUnchecked> =
            change_address.to_string().parse().unwrap();

        self.lock_all_nonwallet_unspents()?;
        let wcfp_result = self.rpc.wallet_create_funded_psbt(
            &[],
            &outputs,
            None,
            Some(WalletCreateFundedPsbtOptions {
                include_watching: Some(true),
                change_address: Some(change_addrs_uncheked),
                fee_rate: Some(Amount::from_sat(fee_rate)),
                ..Default::default()
            }),
            None,
        )?;
        //TODO rust-bitcoin handles psbt, use those functions instead
        let decoded_psbt = self
            .rpc
            .call::<Value>("decodepsbt", &[Value::String(wcfp_result.psbt)])?;
        log::debug!(target: "wallet", "total tx decoded_psbt = {:?}", decoded_psbt);

        let total_tx_inputs_len = decoded_psbt["inputs"].as_array().unwrap().len();
        log::debug!(target: "wallet", "total tx inputs.len = {}", total_tx_inputs_len);
        if total_tx_inputs_len < destinations.len() {
            return Err(WalletError::Protocol(
                "not enough UTXOs found, cant use this method".to_string(),
            ));
        }

        let mut total_tx_inputs = decoded_psbt["tx"]["vin"]
            .as_array()
            .unwrap()
            .iter()
            .zip(decoded_psbt["inputs"].as_array().unwrap().iter())
            .collect::<Vec<(&Value, &Value)>>();

        total_tx_inputs.sort_by(|(_, a), (_, b)| {
            b["witness_utxo"]["amount"]
                .as_f64()
                .unwrap()
                .partial_cmp(&a["witness_utxo"]["amount"].as_f64().unwrap())
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        self.create_mostly_sweep_txes_with_one_tx_having_change(
            coinswap_amount,
            destinations,
            fee_rate,
            &change_address,
            &mut total_tx_inputs.iter().map(|(vin, input_info)| {
                (
                    vin["txid"].as_str().unwrap().parse::<Txid>().unwrap(),
                    vin["vout"].as_u64().unwrap() as u32,
                    convert_json_rpc_bitcoin_to_satoshis(&input_info["witness_utxo"]["amount"]),
                )
            }),
        )
    }

    fn create_funding_txes_use_biggest_utxos(
        &self,
        coinswap_amount: u64,
        destinations: &[Address],
        fee_rate: u64,
    ) -> Result<Option<CreateFundingTxesResult>, WalletError> {
        //this function will pick the top most valuable UTXOs and use them
        //to create funding transactions
        let mut seed_coin_utxo = self.list_descriptor_utxo_unspend_from_wallet()?;
        let mut swap_coin_utxo = self.list_swap_coin_unspend_from_wallet()?;
        seed_coin_utxo.append(&mut swap_coin_utxo);

        let mut list_unspent_result = seed_coin_utxo;
        if list_unspent_result.len() < destinations.len() {
            return Err(WalletError::Protocol(
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
            if sum > coinswap_amount {
                list_unspent_count = Some(ii);
                break;
            }
        }
        if list_unspent_count.is_none() {
            return Err(WalletError::Protocol(
                "Not enough UTXOs/value to create funding txes".to_string(),
            ));
        }

        let inputs = &list_unspent_result[..list_unspent_count.unwrap()];
        log::debug!(target: "wallet", "inputs sizes = {:?}",
            inputs.iter().map(|(l, _)| l.amount.to_sat()).collect::<Vec<u64>>());

        if inputs[1..]
            .iter()
            .map(|(l, _)| l.amount.to_sat())
            .any(|utxo_value| utxo_value > coinswap_amount)
        {
            //at least two utxos bigger than the coinswap amount

            //not implemented yet!
            log::debug!(target: "wallet",
                concat!("Failed to create funding txes with the biggest-utxos method, this ",
                    "branch not implemented yet!"));
            Ok(None)
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

//! Send regular Bitcoin payments.
//!
//! This module provides functionality for managing wallet transactions, including the creation of
//! direct sends. It leverages Bitcoin Core's RPC for wallet synchronization and implements various
//! parsing mechanisms for transaction inputs and outputs.

use bitcoin::{
    absolute::LockTime, transaction::Version, Address, Amount, OutPoint, ScriptBuf, Sequence,
    Transaction, TxIn, TxOut, Witness,
};
use bitcoind::bitcoincore_rpc::{json::ListUnspentResultEntry, RawTx, RpcApi};

use crate::wallet::{api::UTXOSpendInfo, FidelityError};

use super::{error::WalletError, swapcoin::SwapCoin, IncomingSwapCoin, OutgoingSwapCoin, Wallet};

/// Represents different destination options for a transaction.
#[derive(Debug, Clone, PartialEq)]
pub enum Destination {
    /// Sweep
    Sweep(Address),
    /// Multi
    Multi(Vec<(Address, Amount)>),
}

impl Wallet {
    /// API to perform spending from wallet UTXOs, including descriptor coins and swap coins.
    ///
    /// The caller needs to specify a list of UTXO data and their corresponding `spend_info`.
    /// These can be extracted using various `list_utxo_*` Wallet APIs.
    ///
    /// ### Note
    /// This function should not be used to spend Fidelity Bonds or contract UTXOs
    /// (e.g., Hashlock or Timelock contracts). These UTXOs will be automatically skipped
    /// and not considered when creating the transaction.
    ///
    /// ### Behavior
    /// - If [Destination::Sweep] is used, the function creates a transaction for the maximum possible
    ///   value to the specified Address.
    /// - If [Destination::Multi] is used, a custom value is sent, and any remaining funds
    ///   are held in a change address, if applicable.
    pub fn spend_from_wallet(
        &mut self,
        feerate: f64,
        destination: Destination,
        coins_to_spend: &[(ListUnspentResultEntry, UTXOSpendInfo)],
    ) -> Result<Transaction, WalletError> {
        log::info!("Creating Direct-Spend from Wallet.");

        let mut coins = Vec::<(ListUnspentResultEntry, UTXOSpendInfo)>::new();

        for coin in coins_to_spend {
            // filter all contract and fidelity utxos.
            if let UTXOSpendInfo::FidelityBondCoin { .. }
            | UTXOSpendInfo::HashlockContract { .. }
            | UTXOSpendInfo::TimelockContract { .. } = coin.1
            {
                log::warn!("Skipping Fidelity Bond or Contract UTXO.");
                continue;
            } else {
                coins.push(coin.to_owned());
            }
        }

        let tx = self.spend_coins(&coins, destination, feerate)?;

        Ok(tx)
    }

    /// Redeem a Fidelity Bond.
    /// This functions creates a spending transaction from the fidelity bond, signs and broadcasts it.
    /// Returns the txid of the spending tx, and mark the bond as spent.
    pub fn redeem_fidelity(&mut self, idx: u32, feerate: f64) -> Result<(), WalletError> {
        let (bond, redeemed) = self
            .store
            .fidelity_bond
            .get(&idx)
            .ok_or(FidelityError::BondDoesNotExist)?;

        if *redeemed {
            log::info!("Fidelity bond already spent.");
            return Ok(());
        }

        let expired_fidelity_spend_info = UTXOSpendInfo::FidelityBondCoin {
            index: idx,
            input_value: bond.amount,
        };

        let change_addr = &self.get_next_internal_addresses(1)?[0];
        let destination = Destination::Sweep(change_addr.clone());

        // Find utxo corresponding to expired fidelity bond.
        let utxo = match self
            .list_fidelity_spend_info()?
            .iter()
            .find(|(_, spend_info)| *spend_info == expired_fidelity_spend_info)
        {
            Some((utxo, _)) => utxo,
            None => {
                // If no UTXO is found for the expired fidelity bond, it means the bond was already spent,
                // but its `redeemed` flag was never updated. This is a known issue where the redemption
                // transaction was broadcasted, but the flag remained unset.
                // As a temporary fix, we mark the bond as redeemed and exit gracefully.
                log::info!("Fidelity bond already spent.");

                let (_, redeemed) = self
                    .store
                    .fidelity_bond
                    .get_mut(&idx)
                    .ok_or(FidelityError::BondDoesNotExist)?;
                *redeemed = true;

                return Ok(());
            }
        }
        .clone();

        let tx = self.spend_coins(
            &vec![(utxo, expired_fidelity_spend_info)],
            destination,
            feerate,
        )?;
        let txid = self.send_tx(&tx)?;

        // TODO: Potential data inconsistency issue
        // If the server crashes after broadcasting the redemption transaction but before updating
        // the `redeemed` flag, the bond will appear unspent on restart. The system will then
        // attempt to create a new redemption transaction, but the UTXO will already be spent,
        // causing an unrecoverable error.
        // Temporary fix: Log the status and mark the bond as redeemed to prevent repeated failures.
        // A more robust solution is needed to ensure atomic updates.
        log::info!("Fidelity redeem transaction broadcasted. txid: {}", txid);

        // No need to wait for confirmation as that will delay the rpc call. Just send back the txid.

        // mark is_spent
        {
            let (_, redeemed) = self
                .store
                .fidelity_bond
                .get_mut(&idx)
                .ok_or(FidelityError::BondDoesNotExist)?;

            *redeemed = true;
        }

        Ok(())
    }

    pub(crate) fn create_timelock_spend(
        &self,
        og_sc: &OutgoingSwapCoin,
        destination_address: &Address,
        feerate: f64,
    ) -> Result<Transaction, WalletError> {
        let all_utxo = self.list_live_timelock_contract_spend_info()?;

        for (utxo, spend_info) in all_utxo {
            if let UTXOSpendInfo::TimelockContract {
                swapcoin_multisig_redeemscript,
                input_value,
            } = spend_info.clone()
            {
                if swapcoin_multisig_redeemscript == og_sc.get_multisig_redeemscript()
                    && input_value == og_sc.contract_tx.output[0].value
                {
                    let destination = Destination::Sweep(destination_address.clone());
                    let coins = vec![(utxo, spend_info)];
                    let tx = self.spend_coins(&coins, destination, feerate)?;
                    return Ok(tx);
                }
            }
        }
        Err(WalletError::General("Contract Does not exist".to_string()))
    }

    #[allow(unused)]
    pub(crate) fn create_hashlock_spend(
        &self,
        ic_sc: &IncomingSwapCoin,
        destination_address: &Address,
        feerate: f64,
    ) -> Result<Transaction, WalletError> {
        let all_utxo = self.list_live_hashlock_contract_spend_info()?;
        for (utxo, spend_info) in all_utxo {
            if let UTXOSpendInfo::HashlockContract {
                swapcoin_multisig_redeemscript,
                input_value,
            } = spend_info.clone()
            {
                if swapcoin_multisig_redeemscript == ic_sc.get_multisig_redeemscript()
                    && input_value == ic_sc.contract_tx.output[0].value
                {
                    let destination = Destination::Sweep(destination_address.clone());
                    let coin = (utxo, spend_info);
                    let coins = vec![coin];
                    let tx = self.spend_coins(&coins, destination, feerate)?;
                    return Ok(tx);
                }
            }
        }
        Err(WalletError::General("Contract Does not exist".to_string()))
    }

    #[allow(unused)]
    pub fn spend_coins(
        &self,
        coins: &Vec<(ListUnspentResultEntry, UTXOSpendInfo)>,
        destination: Destination,
        feerate: f64,
    ) -> Result<Transaction, WalletError> {
        // Set the Anti-Fee-Snipping locktime
        let current_height = self.rpc.get_block_count()?;
        let lock_time = LockTime::from_height(current_height as u32)?;

        let mut tx = Transaction {
            version: Version::TWO,
            lock_time,
            input: vec![],
            output: vec![],
        };

        let mut total_input_value = Amount::ZERO;
        let mut total_witness_size = 0;
        for (utxo_data, spend_info) in coins {
            match spend_info {
                UTXOSpendInfo::SeedCoin { .. } => {
                    tx.input.push(TxIn {
                        previous_output: OutPoint::new(utxo_data.txid, utxo_data.vout),
                        sequence: Sequence::ZERO,
                        witness: Witness::new(),
                        script_sig: ScriptBuf::new(),
                    });
                    total_witness_size += spend_info.estimate_witness_size();
                    total_input_value += utxo_data.amount;
                }
                UTXOSpendInfo::IncomingSwapCoin { .. } | UTXOSpendInfo::OutgoingSwapCoin { .. } => {
                    tx.input.push(TxIn {
                        previous_output: OutPoint::new(utxo_data.txid, utxo_data.vout),
                        sequence: Sequence::ZERO,
                        witness: Witness::new(),
                        script_sig: ScriptBuf::new(),
                    });
                    total_witness_size += spend_info.estimate_witness_size();
                    total_input_value += utxo_data.amount;
                }
                UTXOSpendInfo::FidelityBondCoin { index, input_value } => {
                    let (bond, redeemed) = self
                        .store
                        .fidelity_bond
                        .get(index)
                        .ok_or(FidelityError::BondDoesNotExist)?;

                    if *redeemed {
                        return Err(FidelityError::BondAlreadyRedeemed.into());
                    }

                    tx.input.push(TxIn {
                        previous_output: bond.outpoint,
                        sequence: Sequence::ZERO,
                        script_sig: ScriptBuf::new(),
                        witness: Witness::new(),
                    });
                    total_witness_size += spend_info.estimate_witness_size();
                    total_input_value += *input_value;
                }
                UTXOSpendInfo::TimelockContract {
                    swapcoin_multisig_redeemscript,
                    input_value,
                } => {
                    let outgoing_swap_coin = self
                        .find_outgoing_swapcoin(swapcoin_multisig_redeemscript)
                        .expect("Cannot find Outgoin Swap Coin");
                    tx.input.push(TxIn {
                        previous_output: OutPoint {
                            txid: outgoing_swap_coin.contract_tx.compute_txid(),
                            vout: 0,
                        },
                        sequence: Sequence(outgoing_swap_coin.get_timelock()? as u32),
                        witness: Witness::new(),
                        script_sig: ScriptBuf::new(),
                    });
                    total_witness_size += spend_info.estimate_witness_size();
                    total_input_value += *input_value;
                }
                UTXOSpendInfo::HashlockContract {
                    swapcoin_multisig_redeemscript,
                    input_value,
                } => {
                    let incoming_swap_coin = self
                        .find_incoming_swapcoin(swapcoin_multisig_redeemscript)
                        .expect("Cannot find Incoming Swap Coin");
                    tx.input.push(TxIn {
                        previous_output: OutPoint {
                            txid: incoming_swap_coin.contract_tx.compute_txid(),
                            vout: 0,
                        },
                        sequence: Sequence(1),
                        witness: Witness::new(),
                        script_sig: ScriptBuf::new(),
                    });
                    total_witness_size += spend_info.estimate_witness_size();
                    total_input_value += *input_value;
                }
            }
        }

        match destination {
            Destination::Sweep(addr) => {
                // Send Max Amount case
                let txout = TxOut {
                    script_pubkey: addr.script_pubkey(),
                    value: Amount::ZERO, // Temp Value
                };
                tx.output.push(txout);
                let base_size = tx.base_size();
                let vsize = (base_size * 4 + total_witness_size).div_ceil(4);

                let fee = Amount::from_sat((feerate * vsize as f64).ceil() as u64);

                #[cfg(feature = "integration-test")]
                let fee =
                    // Timelock spend has hardcoded fees 128 * 2 sats for testcases
                    if coins.len() == 1 && matches!(coins[0].1, UTXOSpendInfo::TimelockContract{..}) {
                        Amount::from_sat(256)
                    }
                    // Otherwise for all the testcases fees will be 1000 sats
                    else {
                        Amount::from_sat(1000)
                    };

                // I don't know if this case is even possible?
                if fee > total_input_value {
                    return Err(WalletError::InsufficientFund {
                        available: total_input_value.to_sat(),
                        required: fee.to_sat(),
                    });
                }

                log::info!("Fee: {} sats", fee.to_sat());
                tx.output[0].value = total_input_value - fee;
            }
            Destination::Multi(addresses) => {
                let mut total_output_value = Amount::ZERO;
                for (address, amount) in addresses {
                    total_output_value += amount;
                    let txout = TxOut {
                        script_pubkey: address.script_pubkey(),
                        value: amount,
                    };
                    tx.output.push(txout);
                }
                let internal_spk = self.get_next_internal_addresses(1)?[0].script_pubkey();
                let minimal_nondust = internal_spk.minimal_non_dust();

                let mut tx_wchange = tx.clone();
                tx_wchange.output.push(TxOut {
                    value: Amount::ZERO, // Adjusted later
                    script_pubkey: internal_spk.clone(),
                });

                let base_wchange = tx_wchange.base_size();
                let vsize_wchange = (base_wchange * 4 + total_witness_size).div_ceil(4);

                let fee_wchange = Amount::from_sat((feerate * vsize_wchange as f64).ceil() as u64);

                #[cfg(feature = "integration-test")]
                let fee_wchange = Amount::from_sat(1000);

                let remaining_wchange =
                    if let Some(diff) = total_input_value.checked_sub(total_output_value) {
                        if let Some(diff) = diff.checked_sub(fee_wchange) {
                            diff
                        } else {
                            return Err(WalletError::InsufficientFund {
                                available: total_input_value.to_sat(),
                                required: (total_output_value + fee_wchange).to_sat(),
                            });
                        }
                    } else {
                        return Err(WalletError::InsufficientFund {
                            available: total_input_value.to_sat(),
                            required: (total_output_value + fee_wchange).to_sat(),
                        });
                    };

                if remaining_wchange > minimal_nondust {
                    log::info!(
                        "Adding change output with {} sats (fee: {} sats)",
                        remaining_wchange.to_sat(),
                        fee_wchange.to_sat()
                    );
                    tx.output.push(TxOut {
                        script_pubkey: internal_spk,
                        value: remaining_wchange,
                    });
                } else {
                    log::info!(
                        "Remaining change {} sats is below dust threshold. Skipping change output. (fee: {} sats)",
                        remaining_wchange.to_sat(),
                        fee_wchange.to_sat()
                    );
                }
            }
        }

        self.sign_transaction(&mut tx, &mut coins.iter().map(|(_, usi)| usi.clone()))?;
        let calc_vsize = (tx.base_size() * 4 + total_witness_size).div_ceil(4);
        let signed_tx_vsize = tx.vsize();

        // As signature size can vary between 71-73 bytes we have a tolerance
        let tolerance_per_input = 2; // Allow a 2-byte difference per input
        let total_tolerance = tolerance_per_input * tx.input.len();

        assert!(
            (calc_vsize as isize - signed_tx_vsize as isize).abs() <= total_tolerance as isize,
            "Calculated vsize {} didn't match signed tx vsize {} (tolerance: {})",
            calc_vsize,
            signed_tx_vsize,
            total_tolerance
        );

        log::debug!("Signed Transaction : {:?}", tx.raw_hex());
        Ok(tx)
    }
}

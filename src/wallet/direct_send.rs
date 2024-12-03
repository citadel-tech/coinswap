//! Send regular Bitcoin payments.
//!
//! This module provides functionality for managing wallet transactions, including the creation of
//! direct sends. It leverages Bitcoin Core's RPC for wallet synchronization and implements various
//! parsing mechanisms for transaction inputs and outputs.

use std::{num::ParseIntError, str::FromStr};

use bitcoin::{
    absolute::LockTime, transaction::Version, Address, Amount, Network, OutPoint, ScriptBuf,
    Sequence, Transaction, TxIn, TxOut, Witness,
};
use bitcoind::bitcoincore_rpc::{json::ListUnspentResultEntry, RawTx, RpcApi};

use crate::wallet::api::UTXOSpendInfo;

use super::{error::WalletError, Wallet};

/// Enum representing different options for the amount to be sent in a transaction.
#[derive(Debug, Clone, PartialEq)]
pub enum SendAmount {
    Max,
    Amount(Amount),
}

impl FromStr for SendAmount {
    type Err = ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(if s == "max" {
            SendAmount::Max
        } else {
            SendAmount::Amount(Amount::from_sat(String::from(s).parse::<u64>()?))
        })
    }
}

/// Enum representing different destination options for a transaction.
#[derive(Debug, Clone, PartialEq)]
pub enum Destination {
    Wallet,
    Address(Address),
}

impl FromStr for Destination {
    type Err = bitcoin::address::ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(if s == "wallet" {
            Destination::Wallet
        } else {
            Destination::Address(Address::from_str(s)?.assume_checked())
        })
    }
}

impl Wallet {
    /// API to perform spending from wallet UTXOs, including descriptor coins and swap coins.
    ///
    /// The caller needs to specify a list of UTXO data and their corresponding `spend_info`.
    /// These can be extracted using various `list_utxo_*` Wallet APIs.
    ///
    /// The caller must also specify a total fee and a destination address.
    /// Using [Destination::Wallet] will create a transaction to an internal wallet change address.
    ///
    /// ### Note
    /// This function should not be used to spend Fidelity Bonds or contract UTXOs
    /// (e.g., Hashlock or Timelock contracts). These UTXOs will be automatically skipped
    /// and not considered when creating the transaction.
    ///
    /// ### Behavior
    /// - If [SendAmount::Max] is used, the function creates a transaction for the maximum possible
    ///   value to the specified destination.
    /// - If [SendAmount::Amount] is used, a custom value is sent, and any remaining funds
    ///    are held in a change address, if applicable.
    pub fn spend_from_wallet(
        &mut self,
        fee: Amount,
        send_amount: SendAmount,
        destination: Destination,
        coins_to_spend: &[(ListUnspentResultEntry, UTXOSpendInfo)],
    ) -> Result<Transaction, WalletError> {
        log::info!("Creating Direct-Spend from Wallet.");

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

        for (utxo_data, spend_info) in coins_to_spend {
            // filter all contract and fidelity utxos.
            if let UTXOSpendInfo::FidelityBondCoin { .. }
            | UTXOSpendInfo::HashlockContract { .. }
            | UTXOSpendInfo::TimelockContract { .. } = spend_info
            {
                log::warn!("Skipping Fidelity Bond or Contract UTXO.");
                continue;
            }

            tx.input.push(TxIn {
                previous_output: OutPoint::new(utxo_data.txid, utxo_data.vout),
                sequence: Sequence::ZERO,
                witness: Witness::new(),
                script_sig: ScriptBuf::new(),
            });

            total_input_value += utxo_data.amount;
        }

        if let SendAmount::Amount(a) = send_amount {
            if a + fee > total_input_value {
                return Err(WalletError::InsufficientFund {
                    available: total_input_value.to_btc(),
                    required: (a + fee).to_btc(),
                });
            }
        }

        log::info!("Total Input Amount: {} | Fees: {}", total_input_value, fee);

        let dest_addr = match destination {
            Destination::Wallet => self.get_next_internal_addresses(1)?[0].clone(),
            Destination::Address(a) => {
                //testnet and signet addresses have the same vbyte
                //so a.network is always testnet even if the address is signet
                let testnet_signet_type = (a.as_unchecked().is_valid_for_network(Network::Testnet)
                    || a.as_unchecked().is_valid_for_network(Network::Signet))
                    && (self.store.network == Network::Testnet
                        || self.store.network == Network::Signet);
                if !a.as_unchecked().is_valid_for_network(self.store.network)
                    && !testnet_signet_type
                {
                    return Err(WalletError::General(
                        "Wrong address type in destinations.".to_string(),
                    ));
                }
                a
            }
        };

        let txout = {
            let value = match send_amount {
                SendAmount::Max => total_input_value - fee,

                SendAmount::Amount(a) => a,
            };
            log::info!("Sending {} to {}.", value, dest_addr);
            TxOut {
                script_pubkey: dest_addr.script_pubkey(),
                value,
            }
        };

        tx.output.push(txout);

        // Only include change if remaining > dust
        if let SendAmount::Amount(amount) = send_amount {
            let internal_spk = self.get_next_internal_addresses(1)?[0].script_pubkey();
            let remaining = total_input_value - amount - fee;
            if remaining > internal_spk.minimal_non_dust() {
                log::info!("Adding Change {}: {}", internal_spk, remaining);
                tx.output.push(TxOut {
                    script_pubkey: internal_spk,
                    value: remaining,
                });
            } else {
                log::info!(
                    "Remaining change {} sats is below dust threshold. Skipping change output.",
                    remaining
                );
            }
        }

        self.sign_transaction(
            &mut tx,
            &mut coins_to_spend.iter().map(|(_, usi)| usi.clone()),
        )?;
        log::debug!("Signed Transaction : {:?}", tx.raw_hex());
        Ok(tx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_send_amount_parsing() {
        assert_eq!(SendAmount::from_str("max").unwrap(), SendAmount::Max);
        assert_eq!(
            SendAmount::from_str("1000").unwrap(),
            SendAmount::Amount(Amount::from_sat(1000))
        );
        assert_ne!(
            SendAmount::from_str("1000").unwrap(),
            SendAmount::from_str("100").unwrap()
        );
        assert!(SendAmount::from_str("not a number").is_err());
    }

    #[test]
    fn test_destination_parsing() {
        assert_eq!(
            Destination::from_str("wallet").unwrap(),
            Destination::Wallet
        );
        let address1 = "32iVBEu4dxkUQk9dJbZUiBiQdmypcEyJRf";
        assert!(matches!(
            Destination::from_str(address1),
            Ok(Destination::Address(_))
        ));

        let address1 = Destination::Address(
            Address::from_str("32iVBEu4dxkUQk9dJbZUiBiQdmypcEyJRf")
                .unwrap()
                .assume_checked(),
        );

        let address2 = Destination::Address(
            Address::from_str("132F25rTsvBdp9JzLLBHP5mvGY66i1xdiM")
                .unwrap()
                .assume_checked(),
        );
        assert_ne!(address1, address2);
        assert!(Destination::from_str("invalid address").is_err());
    }
}

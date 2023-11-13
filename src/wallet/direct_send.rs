use std::{num::ParseIntError, str::FromStr};

use bitcoin::{
    absolute::LockTime, Address, Amount, Network, OutPoint, ScriptBuf, Sequence, Transaction, TxIn,
    TxOut, Witness,
};

use crate::wallet::{api::UTXOSpendInfo, SwapCoin};

use super::{error::WalletError, fidelity::get_locktime_from_index, Wallet};

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
pub enum Destination {
    Wallet,
    Address(Address),
}

impl FromStr for Destination {
    type Err = bitcoin::address::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(if s == "wallet" {
            Destination::Wallet
        } else {
            Destination::Address(Address::from_str(s)?.assume_checked())
        })
    }
}

#[derive(Debug, Clone)]
pub enum CoinToSpend {
    LongForm(OutPoint),
    ShortForm {
        prefix: String,
        suffix: String,
        vout: u32,
    },
}

fn parse_short_form_coin(s: &str) -> Option<CoinToSpend> {
    //example short form: 568a4e..83a2e8:0
    if s.len() < 15 {
        return None;
    }
    let dots = &s[6..8];
    if dots != ".." {
        return None;
    }
    let colon = s.chars().nth(14).unwrap();
    if colon != ':' {
        return None;
    }
    let prefix = String::from(&s[0..6]);
    let suffix = String::from(&s[8..14]);
    let vout = s[15..].parse::<u32>().ok()?;
    Some(CoinToSpend::ShortForm {
        prefix,
        suffix,
        vout,
    })
}

impl FromStr for CoinToSpend {
    type Err = bitcoin::blockdata::transaction::ParseOutPointError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parsed_outpoint = OutPoint::from_str(s);
        if let Ok(op) = parsed_outpoint {
            Ok(CoinToSpend::LongForm(op))
        } else {
            let short_form = parse_short_form_coin(s);
            if let Some(cointospend) = short_form {
                Ok(cointospend)
            } else {
                Err(parsed_outpoint.err().unwrap())
            }
        }
    }
}

impl Wallet {
    pub fn create_direct_send(
        &mut self,
        fee_rate: u64,
        send_amount: SendAmount,
        destination: Destination,
        coins_to_spend: &[CoinToSpend],
    ) -> Result<Transaction, WalletError> {
        let mut tx_inputs = Vec::<TxIn>::new();
        let mut unspent_inputs = Vec::new();
        //TODO this search within a search could get very slow
        let list_unspent_result = self.list_unspent_from_wallet(true, true)?;
        for (list_unspent_entry, spend_info) in list_unspent_result {
            for cts in coins_to_spend {
                let previous_output = match cts {
                    CoinToSpend::LongForm(outpoint) => {
                        if list_unspent_entry.txid == outpoint.txid
                            && list_unspent_entry.vout == outpoint.vout
                        {
                            *outpoint
                        } else {
                            continue;
                        }
                    }
                    CoinToSpend::ShortForm {
                        prefix,
                        suffix,
                        vout,
                    } => {
                        let txid_hex = list_unspent_entry.txid.to_string();
                        if txid_hex.starts_with(prefix)
                            && txid_hex.ends_with(suffix)
                            && list_unspent_entry.vout == *vout
                        {
                            OutPoint {
                                txid: list_unspent_entry.txid,
                                vout: list_unspent_entry.vout,
                            }
                        } else {
                            continue;
                        }
                    }
                };
                log::debug!("found coin to spend = {:?}", previous_output);

                let sequence = match spend_info {
                    UTXOSpendInfo::TimelockContract {
                        ref swapcoin_multisig_redeemscript,
                        input_value: _,
                    } => self
                        .find_outgoing_swapcoin(swapcoin_multisig_redeemscript)
                        .unwrap()
                        .get_timelock() as u32,
                    UTXOSpendInfo::HashlockContract {
                        swapcoin_multisig_redeemscript: _,
                        input_value: _,
                    } => 1, //hashlock spends must have 1 because of the `OP_CSV 1`
                    _ => 0,
                };
                tx_inputs.push(TxIn {
                    previous_output,
                    sequence: Sequence(sequence),
                    witness: Witness::new(),
                    script_sig: ScriptBuf::new(),
                });
                unspent_inputs.push((list_unspent_entry.clone(), spend_info.clone()));
            }
        }
        if tx_inputs.len() != coins_to_spend.len() {
            panic!(
                "unable to find all given inputs, only found = {:?}",
                tx_inputs
            );
        }

        let dest_addr = match destination {
            Destination::Wallet => self.get_next_external_address()?,
            Destination::Address(a) => {
                //testnet and signet addresses have the same vbyte
                //so a.network is always testnet even if the address is signet
                let testnet_signet_type = (a.network == Network::Testnet
                    || a.network == Network::Signet)
                    && (self.store.network == Network::Testnet
                        || self.store.network == Network::Signet);
                if a.network != self.store.network && !testnet_signet_type {
                    panic!("wrong address network type (e.g. mainnet, testnet, regtest, signet)");
                }
                a
            }
        };
        let miner_fee = 500 * fee_rate / 1000; //TODO this is just a rough estimate now

        let mut output = Vec::<TxOut>::new();
        let total_input_value = unspent_inputs
            .iter()
            .fold(Amount::ZERO, |acc, u| acc + u.0.amount)
            .to_sat();
        output.push(TxOut {
            script_pubkey: dest_addr.script_pubkey(),
            value: match send_amount {
                SendAmount::Max => total_input_value - miner_fee,
                SendAmount::Amount(a) => a.to_sat(),
            },
        });
        if let SendAmount::Amount(amount) = send_amount {
            output.push(TxOut {
                script_pubkey: self.get_next_internal_addresses(1)?[0].script_pubkey(),
                value: total_input_value - amount.to_sat() - miner_fee,
            });
        }

        let lock_time = unspent_inputs
            .iter()
            .map(|(_, spend_info)| {
                if let UTXOSpendInfo::FidelityBondCoin {
                    index,
                    input_value: _,
                } = spend_info
                {
                    get_locktime_from_index(*index) as u32 + 1
                } else {
                    0 //TODO add anti-fee-sniping here
                }
            })
            .max()
            .unwrap();

        let mut tx = Transaction {
            input: tx_inputs,
            output,
            lock_time: LockTime::from_time(lock_time).unwrap(),
            version: 2,
        };
        log::debug!("unsigned transaction = {:#?}", tx);
        self.sign_transaction(
            &mut tx,
            &mut unspent_inputs.iter().map(|(_u, usi)| usi.clone()),
        );
        Ok(tx)
    }
}

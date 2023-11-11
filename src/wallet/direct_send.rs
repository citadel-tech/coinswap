use std::{num::ParseIntError, str::FromStr};

use bitcoin::{
    absolute::LockTime, Address, Amount, Network, OutPoint, ScriptBuf, Sequence, Transaction, TxIn,
    TxOut, Witness,
};

use crate::wallet::{wallet::UTXOSpendInfo, SwapCoin};

use super::{error::WalletError, fidelity::get_locktime_from_index, Wallet};

#[derive(Debug, Clone, Eq)]
pub enum SendAmount {
    Max,
    Amount(Amount),
}

impl PartialEq for SendAmount {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (SendAmount::Max, SendAmount::Max) => true,
            (SendAmount::Amount(amount1), SendAmount::Amount(amount2)) => amount1 == amount2,
            _ => false,
        }
    }
    fn ne(&self, other: &Self) -> bool {
        !(self == other)
    }
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

#[derive(Debug, Clone, Eq)]
pub enum Destination {
    Wallet,
    Address(Address),
}

impl PartialEq for Destination {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Destination::Wallet, Destination::Wallet) => true,
            (Destination::Address(a), Destination::Address(b)) => a == b,
            _ => false,
        }
    }

    fn ne(&self, other: &Self) -> bool {
        return !(self == other);
    }
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

#[derive(Debug, Clone, Eq)]
pub enum CoinToSpend {
    LongForm(OutPoint),
    ShortForm {
        prefix: String,
        suffix: String,
        vout: u32,
    },
}

impl PartialEq for CoinToSpend {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (CoinToSpend::LongForm(a), CoinToSpend::LongForm(b)) => a == b,
            (
                CoinToSpend::ShortForm {
                    prefix: a_prefix,
                    suffix: a_suffix,
                    vout: a_vout,
                },
                CoinToSpend::ShortForm {
                    prefix: b_prefix,
                    suffix: b_suffix,
                    vout: b_vout,
                },
            ) => a_prefix == b_prefix && a_suffix == b_suffix && a_vout == b_vout,
            _ => false,
        }
    }
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

#[cfg(test)]
mod tests {

    use crate::wallet::RPCConfig;

    use super::*;
    use bip39::Mnemonic;
    use bitcoin::{Address, Amount};
    use bitcoind::tempfile::tempdir;

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

    #[test]
    fn test_coin_to_spend_long_form_and_short_form_parsing() {
        let valid_outpoint_str =
            "5df6e0e2761359d30a8275058e299fcc0381534545f55cf43e41983f5d4c9456:0";
        let coin_to_spend_long_form = CoinToSpend::LongForm(OutPoint {
            txid: "5df6e0e2761359d30a8275058e299fcc0381534545f55cf43e41983f5d4c9456"
                .parse()
                .unwrap(),
            vout: 0,
        });
        assert_eq!(
            CoinToSpend::from_str(valid_outpoint_str).unwrap(),
            coin_to_spend_long_form
        );
        let valid_outpoint_str =
            "5df6e0e2761359d30a8275058e299fcc0381534545f55cf43e41983f5d4c9456:1";
        assert_ne!(
            CoinToSpend::from_str(valid_outpoint_str).unwrap(),
            coin_to_spend_long_form
        );

        let valid_short_form_str = "123abc..def456:0";
        assert!(matches!(
            CoinToSpend::from_str(valid_short_form_str),
            Ok(CoinToSpend::ShortForm { .. })
        ));
        let mut invalid_short_form_str = "123ab..def456:0";
        assert!(CoinToSpend::from_str(invalid_short_form_str).is_err());

        invalid_short_form_str = "123abc.def456:0";
        assert!(CoinToSpend::from_str(invalid_short_form_str).is_err());

        invalid_short_form_str = "123abc..def4560";
        assert!(CoinToSpend::from_str(invalid_short_form_str).is_err());

        assert!(CoinToSpend::from_str("invalid").is_err());
    }

    #[test]
    fn test_create_direct_send() {
        let temp_dir = tempdir().unwrap();
        let file_path = temp_dir.path().join("test_wallet.json");
        let mnemonic = Mnemonic::generate(12).unwrap().to_string();
        let mut mock_wallet = Wallet::init(
            &file_path,
            &RPCConfig::default(),
            mnemonic,
            "passphrase".to_string(),
        )
        .unwrap();
        let fee_rate = 1000;
        let send_amount = SendAmount::Max;
        let destination = Destination::Wallet;
        let coins_to_spend = [CoinToSpend::LongForm(OutPoint {
            txid: "5df6e0e2761359d30a8275058e299fcc0381534545f55cf43e41983f5d4c9456"
                .parse()
                .unwrap(),
            vout: 0,
        })];

        let result =
            mock_wallet.create_direct_send(fee_rate, send_amount, destination, &coins_to_spend);
        assert!(result.is_err());
    }
}

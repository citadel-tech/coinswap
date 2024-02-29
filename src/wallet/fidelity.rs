use std::{
    collections::HashMap,
    str::FromStr,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use bitcoin::{
    absolute::LockTime,
    bip32::{ChildNumber, DerivationPath},
    hashes::{sha256d, Hash},
    opcodes,
    script::{Builder, Instruction},
    secp256k1::{KeyPair, Message, Secp256k1},
    Address, Amount, OutPoint, PublicKey, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid,
    Witness,
};
use bitcoind::bitcoincore_rpc::RpcApi;
use serde::{Deserialize, Serialize};

use crate::{
    protocol::messages::FidelityProof,
    utill::redeemscript_to_scriptpubkey,
    wallet::{UTXOSpendInfo, Wallet},
};

use super::WalletError;

// To (strongly) disincentivize Sybil behavior, the value assessment of the bond
// is based on the (time value of the bond)^x here x is the bond_value_exponent,
// where x > 1.
const BOND_VALUE_EXPONENT: f64 = 1.3;

// Interest rate used when calculating the value of fidelity bonds created
// by locking bitcoins in timelocked addresses
// See also:
// https://gist.github.com/chris-belcher/87ebbcbb639686057a389acb9ab3e25b#determining-interest-rate-r
// Set as a real number, i.e. 1 = 100% and 0.01 = 1%
const BOND_VALUE_INTEREST_RATE: f64 = 0.015;

/// Constant representing the derivation path for fidelity addresses.
const FIDELITY_DERIVATION_PATH: &str = "m/84'/0'/0'/2";

/// Error structure defining possible fidelity related errors
#[derive(Debug)]
pub enum FidelityError {
    WrongScriptType,
    BondAlreadyExists(u32),
    BondDoesNotExist,
    BondAlreadySpent,
    CertExpired,
    InsufficientFund { available: u64, required: u64 },
}

// impl From<bitcoin::secp256k1::Error> for FidelityError {
//     fn from(value: bitcoin::secp256k1::Error) -> Self {
//         Self::Secp(value)
//     }
// }

// impl From<bitcoin::bip32::Error> for FidelityError {
//     fn from(value: bitcoin::bip32::Error) -> Self {
//         Self::Bip32(value)
//     }
// }

// impl From<bitcoin::consensus::encode::Error> for FidelityError {
//     fn from(value: bitcoin::consensus::encode::Error) -> Self {
//         Self::Encoding(value)
//     }
// }

// impl From<bitcoin::key::Error> for FidelityError {
//     fn from(value: bitcoin::key::Error) -> Self {
//         Self::WrongPubKeyFormat(value.to_string())
//     }
// }

// ------- Fidelity Helper Scripts -------------

/// Create a Fidelity Timelocked redeemscript.
pub fn fidelity_redeemscript(lock_time: &LockTime, pubkey: &PublicKey) -> ScriptBuf {
    Builder::new()
        .push_lock_time(*lock_time)
        .push_opcode(opcodes::all::OP_CLTV)
        .push_opcode(opcodes::all::OP_DROP)
        .push_key(pubkey)
        .push_opcode(opcodes::all::OP_CHECKSIG)
        .into_script()
}

#[allow(unused)]
/// Reads the locktime from a fidelity redeemscript.
pub fn read_locktime_from_fidelity_script(
    redeemscript: &ScriptBuf,
) -> Result<LockTime, FidelityError> {
    if let Some(Ok(Instruction::PushBytes(locktime_bytes))) = redeemscript.instructions().next() {
        let mut u4slice: [u8; 4] = [0; 4];
        u4slice[..locktime_bytes.len()].copy_from_slice(locktime_bytes.as_bytes());
        Ok(LockTime::from_consensus(u32::from_le_bytes(u4slice)))
    } else {
        Err(FidelityError::WrongScriptType)
    }
}

#[allow(unused)]
/// Reads the public key from a fidelity redeemscript.
fn read_pubkey_from_fidelity_script(redeemscript: &ScriptBuf) -> Result<PublicKey, FidelityError> {
    if let Some(Ok(Instruction::PushBytes(pubkey_bytes))) = redeemscript.instructions().nth(3) {
        Ok(PublicKey::from_slice(pubkey_bytes.as_bytes()).unwrap())
    } else {
        Err(FidelityError::WrongScriptType)
    }
}

/// Calculates the theoretical fidelity bond value. Bond value calculation is described in the doc below.
/// https://gist.github.com/chris-belcher/87ebbcbb639686057a389acb9ab3e25b#financial-mathematics-of-joinmarket-fidelity-bonds
pub fn calculate_fidelity_value(
    value: Amount,          // Bond amount in sats
    locktime: u64,          // Bond locktime timestamp
    confirmation_time: u64, // Confirmation timestamp
    current_time: u64,      // Current timestamp
) -> Amount {
    let sec_in_a_year: f64 = 60.0 * 60.0 * 24.0 * 365.2425; // Gregorian calender year length

    let interest_rate = BOND_VALUE_INTEREST_RATE;
    let lock_period_yr = (locktime - confirmation_time) as f64 / sec_in_a_year;
    let locktime_yr = locktime as f64 / sec_in_a_year;
    let currenttime_yr = current_time as f64 / sec_in_a_year;

    // TODO: This calculation can be simplified
    let exp_rt_m1 = f64::exp_m1(interest_rate * lock_period_yr);
    let exp_rtl_m1 = f64::exp_m1(interest_rate * f64::max(0.0, currenttime_yr - locktime_yr));

    let timevalue = f64::max(0.0, f64::min(1.0, exp_rt_m1) - f64::min(1.0, exp_rtl_m1));

    Amount::from_sat((value.to_sat() as f64 * timevalue).powf(BOND_VALUE_EXPONENT) as u64)
}

/// Structure describing a Fidelity Bond.
/// Fidelity Bonds are described in https://github.com/JoinMarket-Org/joinmarket-clientserver/blob/master/docs/fidelity-bonds.md
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Hash)]
pub struct FidelityBond {
    pub outpoint: OutPoint,
    pub amount: u64,
    pub lock_time: LockTime,
    pub pubkey: PublicKey,
    // Height at which the bond was confirmed.
    pub conf_height: u32,
    // Cert expiry denoted in multiple of difficulty adjustment period (2016 blocks)
    pub cert_expiry: u64,
}

impl FidelityBond {
    /// get the reedemscript for this bond
    pub fn redeem_script(&self) -> ScriptBuf {
        fidelity_redeemscript(&self.lock_time, &self.pubkey)
    }

    /// Get the script_pubkey for this bond.
    pub fn script_pub_key(&self) -> ScriptBuf {
        redeemscript_to_scriptpubkey(&self.redeem_script())
    }

    /// Generate the bond's certificate hash.
    pub fn generate_cert_hash(&self, onion_addr: String) -> sha256d::Hash {
        let cert_msg_str = format!(
            "fidelity-bond-cert|{}|{}|{}|{}|{}|{}",
            self.outpoint, self.pubkey, self.cert_expiry, self.lock_time, self.amount, onion_addr
        );
        let cert_msg = cert_msg_str.as_bytes();
        let mut btc_signed_msg = Vec::<u8>::new();
        btc_signed_msg.extend("\x18Bitcoin Signed Message:\n".as_bytes());
        btc_signed_msg.push(cert_msg.len() as u8);
        btc_signed_msg.extend(cert_msg);
        sha256d::Hash::hash(&btc_signed_msg)
    }
}

// Wallet APIs related to fidelity bonds.
impl Wallet {
    /// Get a reference to the fidelity bond store
    pub fn get_fidelity_bonds(&self) -> &HashMap<u32, (FidelityBond, ScriptBuf, bool)> {
        &self.store.fidelity_bond
    }

    /// Get the highest value fidelity bond. Returns None, if no bond exists.
    pub fn get_highest_fidelity_index(&self) -> Result<Option<u32>, WalletError> {
        Ok(self
            .store
            .fidelity_bond
            .iter()
            .filter_map(|(i, (_, _, is_spent))| {
                if !is_spent {
                    let value = self.calculate_bond_value(*i).unwrap();
                    Some((i, value))
                } else {
                    None
                }
            })
            .max_by(|a, b| a.1.cmp(&b.1))
            .map(|(i, _)| *i))
    }
    /// Get the [KeyPair] for the fidelity bond at given index.
    pub fn get_fidelity_keypair(&self, index: u32) -> Result<KeyPair, WalletError> {
        let secp = Secp256k1::new();

        let derivation_path = DerivationPath::from_str(FIDELITY_DERIVATION_PATH)?;

        let child_index = ChildNumber::Normal { index };

        Ok(self
            .store
            .master_key
            .derive_priv(&secp, &derivation_path)?
            .ckd_priv(&secp, child_index)?
            .to_keypair(&secp))
    }

    /// Derives the fidelity redeemscript from bond values at given index.
    pub fn get_fidelity_reedemscript(&self, index: u32) -> Result<ScriptBuf, WalletError> {
        let (bond, _, _) = self
            .store
            .fidelity_bond
            .get(&index)
            .ok_or(FidelityError::BondDoesNotExist)?;
        Ok(bond.redeem_script())
    }

    /// Get the next fidelity bond address. If no fidelity bond is created
    /// returned address will be derived from index 0, of the [FIDELITY_DERIVATION_PATH]
    pub fn get_next_fidelity_address(
        &self,
        locktime: LockTime,
    ) -> Result<(u32, Address, PublicKey), WalletError> {
        // Check what was the last fidelity address index.
        // Derive a fidelity address
        let next_index = self
            .store
            .fidelity_bond
            .keys()
            .map(|i| *i + 1)
            .last()
            .unwrap_or(0);

        let fidelity_pubkey = PublicKey {
            compressed: true,
            inner: self.get_fidelity_keypair(next_index)?.public_key(),
        };

        Ok((
            next_index,
            Address::p2wsh(
                fidelity_redeemscript(&locktime, &fidelity_pubkey).as_script(),
                self.store.network,
            ),
            fidelity_pubkey,
        ))
    }

    /// Calculate the theoretical fidelity bond value.
    /// Bond value calculation is described in the document below.
    /// https://gist.github.com/chris-belcher/87ebbcbb639686057a389acb9ab3e25b#financial-mathematics-of-joinmarket-fidelity-bonds
    pub fn calculate_bond_value(&self, index: u32) -> Result<Amount, WalletError> {
        let (bond, _, _) = self
            .store
            .fidelity_bond
            .get(&index)
            .ok_or(FidelityError::BondDoesNotExist)?;
        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("This can't error")
            .as_secs();

        let hash = self.rpc.get_block_hash(bond.conf_height as u64)?;

        let confirmation_time = self.rpc.get_block_header_info(&hash)?.time as u64;

        let locktime = match bond.lock_time {
            LockTime::Blocks(blocks) => {
                let tip_hash = self.rpc.get_blockchain_info()?.best_block_hash;
                let (tip_height, tip_time) = {
                    let info = self.rpc.get_block_header_info(&tip_hash)?;
                    (info.height, info.time as u64)
                };
                // Estimated locktime from block height = [current-time + (maturity-height - block-count) * 10 * 60] sec
                tip_time + ((blocks.to_consensus_u32() - tip_height as u32) * 10 * 60) as u64
            }
            LockTime::Seconds(sec) => sec.to_consensus_u32() as u64,
        };

        let bond_value = calculate_fidelity_value(
            Amount::from_sat(bond.amount),
            locktime,
            confirmation_time,
            current_time,
        );

        Ok(bond_value)
    }

    /// Create a new fidelity bond with given amount and locktime.
    /// This functions creates the fidelity transaction, signs and broadcast it.
    /// Upon confirmation it stores the fidelity information in the wallet data.
    pub fn create_fidelity(
        &mut self,
        amount: Amount,
        locktime: LockTime, // The final locktime in blockheight or timestamp
    ) -> Result<u32, WalletError> {
        let (index, fidelity_addr, fidelity_pubkey) = self.get_next_fidelity_address(locktime)?;

        // Fetch utxos, filter out existing fidelity coins
        let mut unspents = self
            .list_unspent_from_wallet(false, false)?
            .into_iter()
            .filter(|(_, spend_info)| !matches!(spend_info, UTXOSpendInfo::FidelityBondCoin { .. }))
            .collect::<Vec<_>>();

        unspents.sort_by(|a, b| b.0.amount.cmp(&a.0.amount));

        let mut selected_utxo = Vec::new();
        let mut remaining = amount;

        // the simplest largest first coinselection.
        for unspent in unspents {
            if remaining.checked_sub(unspent.0.amount).is_none() {
                selected_utxo.push(unspent);
                break;
            } else {
                remaining -= unspent.0.amount;
                selected_utxo.push(unspent);
            }
        }

        let fee = Amount::from_sat(1000); // TODO: Update this with the feerate

        let total_input_amount = selected_utxo.iter().fold(Amount::ZERO, |acc, (unspet, _)| {
            acc.checked_add(unspet.amount)
                .expect("Amount sum overflowed")
        });

        if total_input_amount < amount {
            return Err(FidelityError::InsufficientFund {
                available: total_input_amount.to_sat(),
                required: amount.to_sat(),
            }
            .into());
        }

        let change_amount = total_input_amount.checked_sub(amount + fee);
        let tx_inputs = selected_utxo
            .iter()
            .map(|(unspent, _)| TxIn {
                previous_output: OutPoint::new(unspent.txid, unspent.vout),
                sequence: Sequence(0),
                witness: Witness::new(),
                script_sig: ScriptBuf::new(),
            })
            .collect::<Vec<_>>();

        let mut tx_outs = vec![TxOut {
            value: amount.to_sat(),
            script_pubkey: fidelity_addr.script_pubkey(),
        }];

        if change_amount.is_some() {
            let change_addrs = self.get_next_internal_addresses(1)?[0].script_pubkey();
            tx_outs.push(TxOut {
                value: change_amount.expect("expected").to_sat(),
                script_pubkey: change_addrs,
            })
        }
        let current_height = self.rpc.get_block_count()?;
        let anti_fee_snipping_locktime = LockTime::from_height(current_height as u32)?;

        let mut tx = Transaction {
            input: tx_inputs,
            output: tx_outs,
            lock_time: anti_fee_snipping_locktime,
            version: 2, // anti-fee-snipping
        };

        let mut input_info = selected_utxo
            .iter()
            .map(|(_, spend_info)| spend_info.clone());
        self.sign_transaction(&mut tx, &mut input_info)?;

        let txid = self.rpc.send_raw_transaction(&tx)?;

        let conf_height = loop {
            if let Ok(get_tx_result) = self.rpc.get_transaction(&txid, None) {
                if let Some(ht) = get_tx_result.info.blockheight {
                    log::info!("Fidelity Bond confirmed at blockheight: {}", ht);
                    break ht;
                } else {
                    log::info!(
                        "Fildelity Transaction {} seen in mempool, waiting for confirmation.",
                        txid
                    );
                    if cfg!(feature = "integration-test") {
                        thread::sleep(Duration::from_secs(1)); // wait for 1 sec in tests
                    } else {
                        thread::sleep(Duration::from_secs(60 * 10)); // wait for 10 mins in prod
                    }

                    continue;
                }
            } else {
                log::info!("Waiting for {} in mempool", txid);
                continue;
            }
        };

        let cert_expiry = self.get_fidelity_expriy()?;

        let bond = FidelityBond {
            outpoint: OutPoint::new(txid, 0),
            amount: amount.to_sat(),
            lock_time: locktime,
            pubkey: fidelity_pubkey,
            conf_height,
            cert_expiry,
        };

        let bond_spk = bond.script_pub_key();

        self.store
            .fidelity_bond
            .insert(index, (bond, bond_spk, false));

        Ok(index)
    }

    /// Redeem a Fidelity Bond.
    /// This functions creates a spending transaction, signs and broadcasts it.
    /// Upon confirmation it marks the bond as `spent` in the wallet data.
    pub fn redeem_fidelity(&mut self, index: u32) -> Result<Txid, WalletError> {
        let (bond, _, is_spent) = self
            .store
            .fidelity_bond
            .get(&index)
            .ok_or(FidelityError::BondDoesNotExist)?;

        if *is_spent {
            return Err(FidelityError::BondAlreadySpent.into());
        }

        // create a spending transaction.
        let txin = TxIn {
            previous_output: bond.outpoint,
            sequence: Sequence(0),
            script_sig: ScriptBuf::new(),
            witness: Witness::new(),
        };

        // TODO take feerate as user input
        let fee = 1000;

        let change_addr = &self.get_next_internal_addresses(1)?[0];

        let txout = TxOut {
            script_pubkey: change_addr.script_pubkey(),
            value: bond.amount - fee,
        };

        let mut tx = Transaction {
            input: vec![txin],
            output: vec![txout],
            lock_time: bond.lock_time,
            version: 2,
        };

        let utxo_spend_info = UTXOSpendInfo::FidelityBondCoin {
            index,
            input_value: bond.amount,
        };

        self.sign_transaction(&mut tx, vec![utxo_spend_info].into_iter())?;

        let txid = self.rpc.send_raw_transaction(&tx)?;

        let conf_height = loop {
            if let Ok(get_tx_result) = self.rpc.get_transaction(&txid, None) {
                if let Some(ht) = get_tx_result.info.blockheight {
                    log::info!("Fidelity Bond confirmed at blockheight: {}", ht);
                    break ht;
                } else {
                    log::info!(
                        "Fildelity Transaction {} seen in mempool, waiting for confirmation.",
                        txid
                    );

                    if cfg!(feature = "integration-test") {
                        thread::sleep(Duration::from_secs(1)); // wait for 1 sec in tests
                    } else {
                        thread::sleep(Duration::from_secs(60 * 10)); // wait for 10 mins in prod
                    }

                    continue;
                }
            } else {
                log::info!("Waiting for {} in mempool", txid);
                continue;
            }
        };

        log::info!(
            "Fidleity spend txid: {}, confirmed at height : {}",
            txid,
            conf_height
        );

        // mark is_spent
        {
            let (_, _, is_spent) = self
                .store
                .fidelity_bond
                .get_mut(&index)
                .ok_or(FidelityError::BondDoesNotExist)?;

            *is_spent = true;
        }

        Ok(txid)
    }

    /// Generate a [FidelityProof] for bond at a given index and a specific onion address.
    pub fn generate_fidelity_proof(
        &self,
        index: u32,
        onion_addr: String,
    ) -> Result<FidelityProof, WalletError> {
        // Generate a fidelity bond proof from the fidelity data.
        let (bond, _, is_spent) = self
            .store
            .fidelity_bond
            .get(&index)
            .ok_or(FidelityError::BondDoesNotExist)?;

        if *is_spent {
            return Err(FidelityError::BondAlreadySpent.into());
        }

        let fidelity_privkey = self.get_fidelity_keypair(index)?.secret_key();

        let cert_hash = bond.generate_cert_hash(onion_addr);

        let secp = Secp256k1::new();
        let cert_sig = secp.sign_ecdsa(
            &Message::from_slice(cert_hash.as_byte_array())?,
            &fidelity_privkey,
        );

        Ok(FidelityProof {
            bond: bond.clone(),
            cert_hash,
            cert_sig,
        })
    }

    /// Verify a [FidelityProof] received from the directory servers.
    pub fn verify_fidelity_proof(
        &self,
        proof: &FidelityProof,
        onion_addr: String,
    ) -> Result<(), WalletError> {
        if self.is_fidelity_expired(&proof.bond)? {
            return Err(FidelityError::CertExpired.into());
        }

        let cert_message =
            Message::from_slice(proof.bond.generate_cert_hash(onion_addr).as_byte_array())?;

        let secp = Secp256k1::new();

        Ok(secp.verify_ecdsa(&cert_message, &proof.cert_sig, &proof.bond.pubkey.inner)?)
    }

    /// Calculate the expiry value. This depends on the current block height.
    pub fn get_fidelity_expriy(&self) -> Result<u64, WalletError> {
        let current_height = self.rpc.get_block_count()?;
        Ok(((current_height + 2/* safety buffer */) / 2016) + 5)
    }

    /// Extend the expiry of a fidelity bond. This is useful for bonds which are close to their expiry.
    pub fn extend_fidelity_expiry(&mut self, index: u32) -> Result<(), WalletError> {
        let cert_expiry = self.get_fidelity_expriy()?;
        let (bond, _, _) = self
            .store
            .fidelity_bond
            .get_mut(&index)
            .ok_or(FidelityError::BondDoesNotExist)?;

        bond.cert_expiry = cert_expiry;

        Ok(())
    }

    /// Checks if the bond has expired.
    pub fn is_fidelity_expired(&self, bond: &FidelityBond) -> Result<bool, WalletError> {
        // Certificate has expired if current height more than the expiry difficulty period target
        // 1 difficulty period = 2016 blocks
        let current_height = self.rpc.get_block_count()?;
        if current_height > bond.cert_expiry * 2016 {
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_fidelity_bond_value_function_behavior() {
        const EPSILON: f64 = 0.000001;
        const YEAR: f64 = 60.0 * 60.0 * 24.0 * 365.2425;

        //the function should be flat anywhere before the locktime ends
        let values = (0..4)
            .map(|y| {
                calculate_fidelity_value(
                    Amount::from_sat(100000000),
                    (6.0 * YEAR) as u64,
                    0,
                    y * YEAR as u64,
                )
                .to_sat() as f64
            })
            .collect::<Vec<f64>>();
        let value_diff = (0..values.len() - 1)
            .map(|i| values[i + 1] - values[i])
            .collect::<Vec<f64>>();
        for v in &value_diff {
            assert!(v.abs() < EPSILON);
        }

        //after locktime, the value should go down
        let values = (0..5)
            .map(|y| {
                calculate_fidelity_value(
                    Amount::from_sat(100000000),
                    (6.0 * YEAR) as u64,
                    0,
                    (6 + y) * YEAR as u64,
                )
                .to_sat() as f64
            })
            .collect::<Vec<f64>>();
        let value_diff = (0..values.len() - 1)
            .map(|i| values[i + 1] - values[i])
            .collect::<Vec<f64>>();
        for v in &value_diff {
            assert!(*v < 0.0);
        }

        //value of a bond goes up as the locktime goes up
        let values = (0..5)
            .map(|y| {
                calculate_fidelity_value(
                    Amount::from_sat(100000000),
                    (y as f64 * YEAR) as u64,
                    0,
                    0,
                )
                .to_sat() as f64
            })
            .collect::<Vec<f64>>();
        let value_ratio = (0..values.len() - 1)
            .map(|i| values[i] / values[i + 1])
            .collect::<Vec<f64>>();
        let value_ratio_diff = (0..value_ratio.len() - 1)
            .map(|i| value_ratio[i] - value_ratio[i + 1])
            .collect::<Vec<f64>>();
        for v in &value_ratio_diff {
            assert!(*v < 0.0);
        }

        //value of a bond locked into the far future is constant, clamped at the value of burned coins
        let values = (0..5)
            .map(|y| {
                calculate_fidelity_value(
                    Amount::from_sat(100000000),
                    ((200 + y) as f64 * YEAR) as u64,
                    0,
                    0,
                )
                .to_sat() as f64
            })
            .collect::<Vec<f64>>();
        let value_diff = (0..values.len() - 1)
            .map(|i| values[i] - values[i + 1])
            .collect::<Vec<f64>>();
        for v in &value_diff {
            assert!(v.abs() < EPSILON);
        }
    }

    #[test]
    fn test_fidelity_bond_values() {
        let value = Amount::from_btc(1.0).unwrap();
        let confirmation_time = 50_000;
        let current_time = 60_000;

        // Following is a (locktime, fidelity_value) tupple series to show how fidelity_value increases with locktimes
        let test_vectors = [
            (55000, 0), // Value is zero for expired timelocks
            (60000, 3020),
            (65000, 5117),
            (70000, 7437),
            (75000, 9940),
            (80000, 12599),
            (85000, 15395),
            (90000, 18313),
            (95000, 21344),
            (100000, 24477),
            (105000, 27706),
            (110000, 31024),
            (115000, 34426),
            (120000, 37908),
            (125000, 41465),
            (130000, 45094),
            (135000, 48792),
            (140000, 52556),
            (145000, 56383),
        ]
        .map(|(lt, val)| (lt as u64, Amount::from_sat(val)));

        for (locktime, fidelity_value) in test_vectors {
            assert_eq!(
                fidelity_value,
                calculate_fidelity_value(value, locktime, confirmation_time, current_time)
            )
        }
    }

    #[test]
    fn test_fidleity_redeemscripts() {
        let test_data = [(("03ffe2b8b46eb21eadc3b535e9f57054213a1775b035faba6c5b3368b3a0ab5a5c", 15000), "02983ab1752103ffe2b8b46eb21eadc3b535e9f57054213a1775b035faba6c5b3368b3a0ab5a5cac"),
        (("031499764842691088897cff51efd85347dd3215912cbb8fb9b121b1da3b15bec8", 30000), "023075b17521031499764842691088897cff51efd85347dd3215912cbb8fb9b121b1da3b15bec8ac"),
        (("022714334f189db14fabd3dd893bbb913b8c3ddff245f7094cdc0b24c2fabb3570", 45000), "03c8af00b17521022714334f189db14fabd3dd893bbb913b8c3ddff245f7094cdc0b24c2fabb3570ac"),
        (("02145a1d2bd118edcb3fe85495192d44e1d09f75ab4f0fe98269f61ff672860dae", 60000), "0360ea00b1752102145a1d2bd118edcb3fe85495192d44e1d09f75ab4f0fe98269f61ff672860daeac"),]
        .map(|((pk, lt), script)| ((PublicKey::from_str(pk).unwrap(), LockTime::from_height(lt).unwrap()), ScriptBuf::from_hex(script).unwrap()));

        for ((pk, lt), script) in test_data {
            assert_eq!(script, fidelity_redeemscript(&lt, &pk));
            assert_eq!(pk, read_pubkey_from_fidelity_script(&script).unwrap());
            assert_eq!(lt, read_locktime_from_fidelity_script(&script).unwrap());
        }
    }
}

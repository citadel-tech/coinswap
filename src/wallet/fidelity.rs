use crate::{
    protocol::messages::FidelityProof,
    utill::{redeemscript_to_scriptpubkey, verify_fidelity_checks, DEFAULT_TX_FEE_RATE},
    wallet::Wallet,
};
use bitcoin::{
    absolute::LockTime,
    bip32::{ChildNumber, DerivationPath},
    hashes::{sha256d, Hash},
    opcodes::all::{OP_CHECKSIGVERIFY, OP_CLTV},
    script::{Builder, Instruction},
    secp256k1::{Keypair, Message, Secp256k1},
    Address, Amount, OutPoint, PublicKey, ScriptBuf, Txid,
};
use bitcoind::bitcoincore_rpc::RpcApi;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    str::FromStr,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use super::{Destination, WalletError};

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
    BondDoesNotExist,
    BondAlreadyRedeemed,
    BondLocktimeExpired,
    CertExpired,
    InvalidCertHash,
    General(String),
}

// ------- Fidelity Helper Scripts -------------

/// Create a Fidelity Timelocked redeemscript.
/// Redeem script used
/// Old script: <locktime> <OP_CLTV> <OP_DROP> <pubkey> <OP_CHECKSIG>
/// The new script drops the extra byte <OP_DROP>
/// New script: <pubkey> <OP_CHECKSIGVERIFY> <locktime> <OP_CLTV>
pub(crate) fn fidelity_redeemscript(lock_time: &LockTime, pubkey: &PublicKey) -> ScriptBuf {
    Builder::new()
        .push_key(pubkey)
        .push_opcode(OP_CHECKSIGVERIFY)
        .push_lock_time(*lock_time)
        .push_opcode(OP_CLTV)
        .into_script()
}

#[allow(unused)]
/// Reads the locktime from a fidelity redeemscript.
fn read_locktime_from_fidelity_script(redeemscript: &ScriptBuf) -> Result<LockTime, FidelityError> {
    if let Some(Ok(Instruction::PushBytes(locktime_bytes))) = redeemscript.instructions().nth(2) {
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
    if let Some(Ok(Instruction::PushBytes(pubkey_bytes))) = redeemscript.instructions().next() {
        Ok(PublicKey::from_slice(pubkey_bytes.as_bytes())
            .map_err(|e| FidelityError::General(e.to_string()))?)
    } else {
        Err(FidelityError::WrongScriptType)
    }
}

/// Calculates the theoretical fidelity bond value. Bond value calculation is described in the doc below.
/// https://gist.github.com/chris-belcher/87ebbcbb639686057a389acb9ab3e25b#financial-mathematics-of-joinmarket-fidelity-bonds
fn calculate_fidelity_value(
    value: Amount,          // Bond amount in sats
    locktime: u64,          // Bond locktime timestamp
    confirmation_time: u64, // Confirmation timestamp
    current_time: u64,      // Current timestamp
) -> Amount {
    let sec_in_a_year: f64 = 60.0 * 60.0 * 24.0 * 365.2425; // Gregorian calender year length

    let interest_rate = BOND_VALUE_INTEREST_RATE;
    let lock_period_yr = ((locktime - confirmation_time) as f64) / sec_in_a_year;
    let locktime_yr = (locktime as f64) / sec_in_a_year;
    let currenttime_yr = (current_time as f64) / sec_in_a_year;

    // TODO: This calculation can be simplified
    let exp_rt_m1 = f64::exp_m1(interest_rate * lock_period_yr);
    let exp_rtl_m1 = f64::exp_m1(interest_rate * f64::max(0.0, currenttime_yr - locktime_yr));

    let timevalue = f64::max(0.0, f64::min(1.0, exp_rt_m1) - f64::min(1.0, exp_rtl_m1));

    Amount::from_sat(((value.to_sat() as f64) * timevalue).powf(BOND_VALUE_EXPONENT) as u64)
}

/// Structure describing a Fidelity Bond.
/// Fidelity Bonds are described in https://github.com/JoinMarket-Org/joinmarket-clientserver/blob/master/docs/fidelity-bonds.md
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Hash)]
pub struct FidelityBond {
    pub(crate) outpoint: OutPoint,
    /// Fidelity Amount
    pub amount: Amount,
    /// Fidelity Locktime
    pub lock_time: LockTime,
    pub(crate) pubkey: PublicKey,
    // Height at which the bond was confirmed.
    pub(crate) conf_height: Option<u32>,
    // Cert expiry denoted in multiple of difficulty adjustment period (2016 blocks)
    pub(crate) cert_expiry: Option<u32>,
}

impl FidelityBond {
    /// get the reedemscript for this bond
    pub(crate) fn redeem_script(&self) -> ScriptBuf {
        fidelity_redeemscript(&self.lock_time, &self.pubkey)
    }

    /// Get the script_pubkey for this bond.
    pub(crate) fn script_pub_key(&self) -> ScriptBuf {
        redeemscript_to_scriptpubkey(&self.redeem_script()).expect("This can never panic as fidelity redeemscript template is hardcoded in a private function.")
    }

    /// Generate the bond's certificate hash.
    pub(crate) fn generate_cert_hash(&self, addr: &str) -> Option<sha256d::Hash> {
        self.cert_expiry.map(|expiry| {
            let cert_msg_str = format!(
                "fidelity-bond-cert|{}|{}|{}|{}|{}|{}",
                self.outpoint, self.pubkey, expiry, self.lock_time, self.amount, addr
            );
            let cert_msg = cert_msg_str.as_bytes();
            let mut btc_signed_msg = Vec::<u8>::new();
            btc_signed_msg.extend("\x18Bitcoin Signed Message:\n".as_bytes());
            btc_signed_msg.push(cert_msg.len() as u8);
            btc_signed_msg.extend(cert_msg);

            sha256d::Hash::hash(&btc_signed_msg)
        })
    }

    /// Calculate the expiry value. This depends on the bond's confirmation height
    pub(crate) fn get_fidelity_expiry(conf_height: u32) -> u32 {
        (conf_height + 2) /* safety buffer */ / 2016 + 5
    }
}

// Wallet APIs related to fidelity bonds.
impl Wallet {
    /// Get a reference to the fidelity bond store
    pub fn get_fidelity_bonds(&self) -> &HashMap<u32, (FidelityBond, bool)> {
        &self.store.fidelity_bond
    }

    /// Display the fidelity bonds
    pub fn display_fidelity_bonds(&self) -> Result<String, WalletError> {
        let serialized = self
            .store
            .fidelity_bond
            .iter()
            .map(|(index, (bond, redeemed))| {
                let mut bond_info = serde_json::json!({
                        "index": index,
                        "outpoint": bond.outpoint.to_string(),
                        "amount": bond.amount.to_sat(),
                        "status": if *redeemed {"Redeemed"} else {"Live"}
                });

                if !*redeemed {
                    let bond_value = self
                        .calculate_bond_value(bond)
                        .expect("Bond value calculation must not fail for valid bonds.");
                    bond_info["bond_value"] = serde_json::json!(bond_value);
                }

                bond_info
            })
            .collect::<Vec<serde_json::Value>>();

        serde_json::to_string_pretty(&serialized).map_err(|e| WalletError::General(e.to_string()))
    }

    /// Get the highest value fidelity bond. Returns None, if no bond exists.
    pub fn get_highest_fidelity_index(&self) -> Result<Option<u32>, WalletError> {
        Ok(self
            .store
            .fidelity_bond
            .iter()
            .filter_map(|(i, (bond, expired))| {
                if !expired {
                    match self.calculate_bond_value(bond) {
                        Ok(v) => {
                            log::info!("Fidelity Bond found | Index: {} | Bond Value : {}", i, v);
                            Some((i, v))
                        }
                        Err(e) => {
                            log::error!("Fidelity valuation failed for index {}:  {:?} ", i, e);
                            None
                        }
                    }
                } else {
                    None
                }
            })
            .max_by(|a, b| a.1.cmp(&b.1))
            .map(|(i, _)| *i))
    }

    /// Get the [KeyPair] for the fidelity bond at given index.
    pub(crate) fn get_fidelity_keypair(&self, index: u32) -> Result<Keypair, WalletError> {
        let secp = Secp256k1::new();

        let derivation_path = DerivationPath::from_str(FIDELITY_DERIVATION_PATH)?;

        let child_derivation_path = derivation_path.child(ChildNumber::Normal { index });

        Ok(self
            .store
            .master_key
            .derive_priv(&secp, &child_derivation_path)?
            .to_keypair(&secp))
    }

    /// Derives the fidelity redeemscript from bond values at given index.
    pub(crate) fn get_fidelity_reedemscript(&self, index: u32) -> Result<ScriptBuf, WalletError> {
        let (bond, _) = self
            .store
            .fidelity_bond
            .get(&index)
            .ok_or(FidelityError::BondDoesNotExist)?;
        Ok(bond.redeem_script())
    }

    /// Get the next fidelity bond address. If no fidelity bond is created
    /// returned address will be derived from index 0, of the [FIDELITY_DERIVATION_PATH]
    pub(crate) fn get_next_fidelity_address(
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
    pub fn calculate_bond_value(&self, bond: &FidelityBond) -> Result<Amount, WalletError> {
        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("This can't error")
            .as_secs();

        let hash = self
            .rpc
            .get_block_hash(bond.conf_height.ok_or(FidelityError::BondDoesNotExist)? as u64)?;

        let confirmation_time = self.rpc.get_block_header_info(&hash)?.time as u64;

        let locktime = match bond.lock_time {
            LockTime::Blocks(blocks) => {
                let tip_hash = self.rpc.get_blockchain_info()?.best_block_hash;
                let (tip_height, tip_time) = {
                    let info = self.rpc.get_block_header_info(&tip_hash)?;
                    (info.height, info.time as u64)
                };
                // Estimated locktime from block height = [current-time + (maturity-height - block-count) * 10 * 60] sec
                let height_diff =
                    if let Some(x) = blocks.to_consensus_u32().checked_sub(tip_height as u32) {
                        x as u64
                    } else {
                        return Err(FidelityError::BondLocktimeExpired.into());
                    };

                tip_time + (height_diff * 10 * 60)
            }
            LockTime::Seconds(sec) => sec.to_consensus_u32() as u64,
        };

        let bond_value =
            calculate_fidelity_value(bond.amount, locktime, confirmation_time, current_time);

        Ok(bond_value)
    }

    /// Create a new fidelity bond with given amount and absolute height based locktime.
    /// This functions creates the fidelity transaction, signs and broadcast it.
    /// Upon confirmation it stores the fidelity information in the wallet data.
    pub fn create_fidelity(
        &mut self,
        amount: Amount,
        locktime: LockTime,
        feerate: f64,
    ) -> Result<u32, WalletError> {
        let (index, fidelity_addr, fidelity_pubkey) = self.get_next_fidelity_address(locktime)?;

        let coins = self.coin_select(amount, feerate)?;

        let destination = Destination::Multi(vec![(fidelity_addr, amount)]);

        let tx = self.spend_coins(&coins, destination, feerate)?;

        let txid = self.send_tx(&tx)?;

        // Register this bond even it is in mempool and not yet confirmed to avoid the edge case when the maker server
        // unexpectedly shutdown while it was waiting for the fidelity transaction confirmation.
        // Otherwise the wallet wouldn't know about this bond in this case and would attempt to create a new bond again.
        {
            let bond = FidelityBond {
                outpoint: OutPoint::new(txid, 0),
                amount,
                lock_time: locktime,
                pubkey: fidelity_pubkey,
                // `Conf_height` & `cert_expiry` are considered None as they can't be known before the confirmation.
                conf_height: None,
                cert_expiry: None,
            };
            self.store.fidelity_bond.insert(index, (bond, false));
            self.save_to_disk()?;
        }

        let conf_height = self.wait_for_fidelity_tx_confirmation(txid)?;

        self.update_fidelity_bond_conf_details(index, conf_height)?;

        Ok(index)
    }

    /// Waits for the fidelity transaction to confirm and returns its block height.  
    pub(crate) fn wait_for_fidelity_tx_confirmation(&self, txid: Txid) -> Result<u32, WalletError> {
        let sleep_increment = 10;
        let mut sleep_multiplier = 0;

        let ht = loop {
            sleep_multiplier += 1;

            let get_tx_result = self.rpc.get_transaction(&txid, None)?;
            if let Some(ht) = get_tx_result.info.blockheight {
                log::info!(
                    "Fidelity Transaction {} confirmed at blockheight: {}",
                    txid,
                    ht
                );
                break ht;
            } else {
                log::info!(
                    "Fidelity Transaction {} seen in mempool, waiting for confirmation.",
                    txid
                );
                let total_sleep = sleep_increment * sleep_multiplier.min(10 * 60); // Caps at 10 minutes
                log::info!("Next sync in {:?} secs", total_sleep);
                thread::sleep(Duration::from_secs(total_sleep));
            }
        };

        Ok(ht)
    }

    pub(crate) fn update_fidelity_bond_conf_details(
        &mut self,
        index: u32,
        conf_height: u32,
    ) -> Result<(), WalletError> {
        let cert_expiry = FidelityBond::get_fidelity_expiry(conf_height);
        let (bond, _) = self
            .store
            .fidelity_bond
            .get_mut(&index)
            .ok_or(FidelityError::BondDoesNotExist)?;

        bond.cert_expiry = Some(cert_expiry);
        bond.conf_height = Some(conf_height);

        self.sync()?;

        Ok(())
    }

    /// Redeems all expired fidelity bonds in the wallet ,if found any.
    pub fn redeem_expired_fidelity_bonds(&mut self) -> Result<(), WalletError> {
        let curr_height = self.rpc.get_block_count()? as u32;

        let expired_bond_indices = self
            .store
            .fidelity_bond
            .iter()
            .filter_map(|(&i, (bond, redeemed))| {
                if !redeemed && curr_height > bond.lock_time.to_consensus_u32() {
                    Some(i)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        expired_bond_indices.into_iter().try_for_each(|i| {
            log::info!("Fidelity Bond at index: {:?} expired | Redeeming it.", i);
            self.redeem_fidelity(i, DEFAULT_TX_FEE_RATE).map(|_| ())
        })
    }

    /// Generate a [FidelityProof] for bond at a given index and a specific onion address.
    pub(crate) fn generate_fidelity_proof(
        &self,
        index: u32,
        maker_addr: &str,
    ) -> Result<FidelityProof, WalletError> {
        // Generate a fidelity bond proof from the fidelity data.
        let (bond, redeemed) = self
            .store
            .fidelity_bond
            .get(&index)
            .ok_or(FidelityError::BondDoesNotExist)?;

        if *redeemed {
            return Err(FidelityError::BondAlreadyRedeemed.into());
        }

        let fidelity_privkey = self.get_fidelity_keypair(index)?.secret_key();

        let cert_hash = bond
            .generate_cert_hash(maker_addr)
            .expect("Bond is not yet confirmed");

        let secp = Secp256k1::new();
        let cert_sig = secp.sign_ecdsa(
            &Message::from_digest_slice(cert_hash.as_byte_array())?,
            &fidelity_privkey,
        );

        Ok(FidelityProof {
            bond: bond.clone(),
            cert_hash,
            cert_sig,
        })
    }

    /// Verify a [FidelityProof] received from the directory servers.
    pub(crate) fn verify_fidelity_proof(
        &self,
        proof: &FidelityProof,
        onion_addr: &str,
    ) -> Result<(), WalletError> {
        let txid = proof.bond.outpoint.txid;
        let transaction = self.rpc.get_raw_transaction(&txid, None)?;
        let current_height = self.rpc.get_block_count()?;

        verify_fidelity_checks(proof, onion_addr, transaction, current_height)
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
                    y * (YEAR as u64),
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
                    (6 + y) * (YEAR as u64),
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
                    ((y as f64) * YEAR) as u64,
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
                    (((200 + y) as f64) * YEAR) as u64,
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
            );
        }
    }
}

#[test]
fn test_fidleity_redeemscripts() {
    let test_data = [
        (
            (
                "03ffe2b8b46eb21eadc3b535e9f57054213a1775b035faba6c5b3368b3a0ab5a5c",
                15000,
            ),
            "2103ffe2b8b46eb21eadc3b535e9f57054213a1775b035faba6c5b3368b3a0ab5a5cad02983ab1",
        ),
        (
            (
                "031499764842691088897cff51efd85347dd3215912cbb8fb9b121b1da3b15bec8",
                30000,
            ),
            "21031499764842691088897cff51efd85347dd3215912cbb8fb9b121b1da3b15bec8ad023075b1",
        ),
        (
            (
                "022714334f189db14fabd3dd893bbb913b8c3ddff245f7094cdc0b24c2fabb3570",
                45000,
            ),
            "21022714334f189db14fabd3dd893bbb913b8c3ddff245f7094cdc0b24c2fabb3570ad03c8af00b1",
        ),
        (
            (
                "02145a1d2bd118edcb3fe85495192d44e1d09f75ab4f0fe98269f61ff672860dae",
                60000,
            ),
            "2102145a1d2bd118edcb3fe85495192d44e1d09f75ab4f0fe98269f61ff672860daead0360ea00b1",
        ),
    ]
    .map(|((pk, lt), script)| {
        (
            (
                PublicKey::from_str(pk).unwrap(),
                LockTime::from_height(lt).unwrap(),
            ),
            ScriptBuf::from_hex(script).unwrap(),
        )
    });

    for ((pk, lt), script) in test_data {
        assert_eq!(script, fidelity_redeemscript(&lt, &pk));
        assert_eq!(pk, read_pubkey_from_fidelity_script(&script).unwrap());
        assert_eq!(lt, read_locktime_from_fidelity_script(&script).unwrap());
    }
}

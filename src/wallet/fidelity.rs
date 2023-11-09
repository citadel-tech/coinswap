// To (strongly) disincentivize Sybil behaviour, the value assessment of the bond
// is based on the (time value of the bond)^x where x is the bond_value_exponent here,
// where x > 1.
const BOND_VALUE_EXPONENT: f64 = 1.3;

// Interest rate used when calculating the value of fidelity bonds created
// by locking bitcoins in timelocked addresses
// See also:
// https://gist.github.com/chris-belcher/87ebbcbb639686057a389acb9ab3e25b#determining-interest-rate-r
// Set as a real number, i.e. 1 = 100% and 0.01 = 1%
const BOND_VALUE_INTEREST_RATE: f64 = 0.015;

use std::{collections::HashMap, fmt::Display, num::ParseIntError, str::FromStr};

use chrono::NaiveDate;

use bitcoin::{
    bip32::{ChildNumber, DerivationPath, ExtendedPrivKey},
    blockdata::{
        opcodes,
        script::{Builder, Instruction, Script},
    },
    hashes::{sha256d, Hash},
    secp256k1::{Context, Message, Secp256k1, SecretKey, Signing},
    Address, OutPoint, PublicKey, ScriptBuf,
};

use bitcoind::bitcoincore_rpc::{
    json::{GetTxOutResult, ListUnspentResultEntry},
    Client, RpcApi,
};

use crate::{
    protocol::messages::FidelityBondProof,
    utill::{generate_keypair, redeemscript_to_scriptpubkey},
    wallet::{UTXOSpendInfo, Wallet},
};

pub const TIMELOCKED_MPK_PATH: &str = "m/84'/0'/0'/2";
pub const TIMELOCKED_ADDRESS_COUNT: u32 = 960;

pub const REGTEST_DUMMY_ONION_HOSTNAME: &str = "regtest-dummy-onion-hostname.onion";

#[derive(Debug, Clone)]
pub struct YearAndMonth {
    year: u32,
    month: u32,
}

impl YearAndMonth {
    pub fn new(year: u32, month: u32) -> YearAndMonth {
        YearAndMonth { year, month }
    }

    pub fn to_index(&self) -> u32 {
        (self.year - 2020) * 12 + (self.month - 1)
    }
}

impl FromStr for YearAndMonth {
    type Err = YearAndMonthError;

    // yyyy-mm
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != 7 {
            return Err(YearAndMonthError::WrongLength);
        }
        let year = String::from(&s[..4]).parse::<u32>()?;
        let month = String::from(&s[5..]).parse::<u32>()?;
        if (2020..=2079).contains(&year) && (1..=12).contains(&month) {
            Ok(YearAndMonth { year, month })
        } else {
            Err(YearAndMonthError::OutOfRange)
        }
    }
}

impl From<std::ffi::OsString> for YearAndMonth {
    fn from(value: std::ffi::OsString) -> Self {
        YearAndMonth::from_str(&value.into_string().unwrap()).unwrap()
    }
}

#[derive(Debug)]
pub enum YearAndMonthError {
    WrongLength,
    ParseIntError(ParseIntError),
    OutOfRange,
}

impl From<ParseIntError> for YearAndMonthError {
    fn from(p: ParseIntError) -> YearAndMonthError {
        YearAndMonthError::ParseIntError(p)
    }
}

impl Display for YearAndMonthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            YearAndMonthError::WrongLength => write!(f, "WrongLength, should be yyyy-mm"),
            YearAndMonthError::ParseIntError(p) => p.fmt(f),
            YearAndMonthError::OutOfRange => {
                write!(f, "Out of range, must be between 2020-01 and 2079-12")
            }
        }
    }
}

fn create_cert_msg_hash(cert_pubkey: &PublicKey, cert_expiry: u16) -> Message {
    let cert_msg_str = format!("fidelity-bond-cert|{}|{}", cert_pubkey, cert_expiry);
    let cert_msg = cert_msg_str.as_bytes();
    let mut btc_signed_msg = Vec::<u8>::new();
    btc_signed_msg.extend("\x18Bitcoin Signed Message:\n".as_bytes());
    btc_signed_msg.push(cert_msg.len() as u8);
    btc_signed_msg.extend(cert_msg);
    Message::from_slice(sha256d::Hash::hash(&btc_signed_msg).as_byte_array()).unwrap()
}

pub struct HotWalletFidelityBond {
    pub utxo: OutPoint,
    utxo_key: PublicKey,
    locktime: i64,
    utxo_privkey: SecretKey,
}

impl HotWalletFidelityBond {
    pub fn new(wallet: &Wallet, utxo: &ListUnspentResultEntry, spend_info: &UTXOSpendInfo) -> Self {
        let index = if let UTXOSpendInfo::FidelityBondCoin {
            index,
            input_value: _,
        } = spend_info
        {
            *index
        } else {
            panic!("bug, should be fidelity bond coin")
        };
        let redeemscript = wallet.get_timelocked_redeemscript_from_index(index);
        Self {
            utxo: OutPoint {
                txid: utxo.txid,
                vout: utxo.vout,
            },
            utxo_key: read_pubkey_from_timelocked_redeemscript(&redeemscript).unwrap(),
            locktime: read_locktime_from_timelocked_redeemscript(&redeemscript).unwrap(),
            utxo_privkey: wallet.get_timelocked_privkey_from_index(index),
        }
    }

    pub fn create_proof(&self, rpc: &Client, onion_hostname: &str) -> FidelityBondProof {
        const BLOCK_COUNT_SAFETY: u64 = 2;
        const RETARGET_INTERVAL: u64 = 2016;
        const CERT_MAX_VALIDITY_TIME: u64 = 1;

        let blocks = rpc.get_block_count().unwrap();
        let cert_expiry =
            ((blocks + BLOCK_COUNT_SAFETY) / RETARGET_INTERVAL) + CERT_MAX_VALIDITY_TIME;
        let cert_expiry = cert_expiry as u16;

        let (cert_pubkey, cert_privkey) = generate_keypair();
        let secp = Secp256k1::new();

        let cert_msg_hash = create_cert_msg_hash(&cert_pubkey, cert_expiry);
        let cert_sig = secp.sign_ecdsa(&cert_msg_hash, &self.utxo_privkey);

        let onion_msg_hash =
            Message::from_slice(sha256d::Hash::hash(onion_hostname.as_bytes()).as_byte_array())
                .unwrap();
        let onion_sig = secp.sign_ecdsa(&onion_msg_hash, &cert_privkey);

        FidelityBondProof {
            utxo: self.utxo,
            utxo_key: self.utxo_key,
            locktime: self.locktime,
            cert_sig,
            cert_expiry,
            cert_pubkey,
            onion_sig,
        }
    }
}

impl FidelityBondProof {
    pub fn verify_and_get_txo(
        &self,
        rpc: &Client,
        block_count: u64,
        onion_hostname: &str,
    ) -> GetTxOutResult {
        let secp = Secp256k1::new();

        let onion_msg_hash =
            Message::from_slice(sha256d::Hash::hash(onion_hostname.as_bytes()).as_byte_array())
                .unwrap();
        secp.verify_ecdsa(&onion_msg_hash, &self.onion_sig, &self.cert_pubkey.inner)
            .unwrap();
        let cert_msg_hash = create_cert_msg_hash(&self.cert_pubkey, self.cert_expiry);
        secp.verify_ecdsa(&cert_msg_hash, &self.cert_sig, &self.utxo_key.inner)
            .unwrap();

        let txo_data = rpc
            .get_tx_out(&self.utxo.txid, self.utxo.vout, None)
            .unwrap()
            .unwrap();

        const RETARGET_INTERVAL: u64 = 2016;
        if block_count > self.cert_expiry as u64 * RETARGET_INTERVAL {
            panic!("cert has expired");
        }

        let implied_spk = redeemscript_to_scriptpubkey(&create_timelocked_redeemscript(
            self.locktime,
            &self.utxo_key,
        ));
        if txo_data.script_pub_key.hex != implied_spk.into_bytes() {
            panic!("UTXO script doesnt match given script",);
        }

        //an important thing we cant verify in this function
        //is that a given fidelity bond UTXO was only used once in the offer book
        //that has to be checked elsewhere

        txo_data
    }

    pub fn calculate_fidelity_bond_value(
        &self,
        rpc: &Client,
        block_count: u64,
        txo_data: &GetTxOutResult,
        mediantime: u64,
    ) -> f64 {
        let blockhash = rpc
            .get_block_hash(block_count - txo_data.confirmations as u64 + 1)
            .unwrap();
        calculate_timelocked_fidelity_bond_value(
            txo_data.value.to_sat(),
            self.locktime,
            rpc.get_block_header_info(&blockhash).unwrap().time as i64,
            mediantime,
        )
    }
}

#[allow(non_snake_case)]
fn calculate_timelocked_fidelity_bond_value(
    value_sats: u64,
    locktime: i64,
    confirmation_time: i64,
    current_time: u64,
) -> f64 {
    const YEAR: f64 = 60.0 * 60.0 * 24.0 * 365.2425; //gregorian calender year length

    let r = BOND_VALUE_INTEREST_RATE;
    let T = (locktime - confirmation_time) as f64 / YEAR;
    let L = locktime as f64 / YEAR;
    let t = current_time as f64 / YEAR;

    let exp_rT_m1 = f64::exp_m1(r * T);
    let exp_rtL_m1 = f64::exp_m1(r * f64::max(0.0, t - L));

    let timevalue = f64::max(0.0, f64::min(1.0, exp_rT_m1) - f64::min(1.0, exp_rtL_m1));

    (value_sats as f64 * timevalue).powf(BOND_VALUE_EXPONENT)
}

fn calculate_timelocked_fidelity_bond_value_from_utxo(
    utxo: &ListUnspentResultEntry,
    usi: &UTXOSpendInfo,
    rpc: &Client,
) -> f64 {
    calculate_timelocked_fidelity_bond_value(
        utxo.amount.to_sat(),
        get_locktime_from_index(
            if let UTXOSpendInfo::FidelityBondCoin {
                index,
                input_value: _,
            } = usi
            {
                *index
            } else {
                panic!("bug, should be fidelity bond coin")
            },
        ),
        rpc.get_transaction(&utxo.txid, Some(true))
            .unwrap()
            .info
            .blocktime
            .unwrap() as i64,
        rpc.get_blockchain_info().unwrap().median_time,
    )
}

fn create_timelocked_redeemscript(locktime: i64, pubkey: &PublicKey) -> ScriptBuf {
    Builder::new()
        .push_int(locktime)
        .push_opcode(opcodes::all::OP_CLTV)
        .push_opcode(opcodes::all::OP_DROP)
        .push_key(pubkey)
        .push_opcode(opcodes::all::OP_CHECKSIG)
        .into_script()
}

pub fn read_locktime_from_timelocked_redeemscript(redeemscript: &Script) -> Option<i64> {
    if let Instruction::PushBytes(locktime_bytes) = redeemscript.instructions().next()?.ok()? {
        let mut u8slice: [u8; 8] = [0; 8];
        u8slice[..locktime_bytes.len()].copy_from_slice(locktime_bytes.as_bytes());
        Some(i64::from_le_bytes(u8slice))
    } else {
        None
    }
}

fn read_pubkey_from_timelocked_redeemscript(redeemscript: &Script) -> Option<PublicKey> {
    if let Instruction::PushBytes(pubkey_bytes) = redeemscript.instructions().nth(3)?.ok()? {
        PublicKey::from_slice(pubkey_bytes.as_bytes()).ok()
    } else {
        None
    }
}

fn get_timelocked_master_key_from_root_master_key(master_key: &ExtendedPrivKey) -> ExtendedPrivKey {
    let secp = Secp256k1::new();

    master_key
        .derive_priv(
            &secp,
            &DerivationPath::from_str(TIMELOCKED_MPK_PATH).unwrap(),
        )
        .unwrap()
}

pub fn get_locktime_from_index(index: u32) -> i64 {
    let year_off = index as i32 / 12;
    let month = index % 12;
    NaiveDate::from_ymd_opt(2020 + year_off, 1 + month, 1)
        .expect("expected")
        .and_hms_opt(0, 0, 0)
        .expect("expected")
        .timestamp()
}

fn get_timelocked_redeemscript_from_index<C: Context + Signing>(
    secp: &Secp256k1<C>,
    timelocked_master_private_key: &ExtendedPrivKey,
    index: u32,
) -> ScriptBuf {
    let privkey = timelocked_master_private_key
        .ckd_priv(secp, ChildNumber::Normal { index })
        .unwrap()
        .private_key;
    let pubkey = PublicKey {
        compressed: true,
        inner: privkey.public_key(secp),
    };
    let locktime = get_locktime_from_index(index);
    create_timelocked_redeemscript(locktime, &pubkey)
}

pub fn generate_fidelity_scripts(master_key: &ExtendedPrivKey) -> HashMap<ScriptBuf, u32> {
    let timelocked_master_private_key = get_timelocked_master_key_from_root_master_key(master_key);
    let mut timelocked_script_index_map = HashMap::new();

    let secp = Secp256k1::new();
    //all these magic numbers and constants are explained in the fidelity bonds bip
    // https://gist.github.com/chris-belcher/7257763cedcc014de2cd4239857cd36e
    for index in 0..TIMELOCKED_ADDRESS_COUNT {
        let redeemscript =
            get_timelocked_redeemscript_from_index(&secp, &timelocked_master_private_key, index);
        let spk = redeemscript_to_scriptpubkey(&redeemscript);
        timelocked_script_index_map.insert(spk, index);
    }
    timelocked_script_index_map
}

impl Wallet {
    pub fn get_timelocked_redeemscript_from_index(&self, index: u32) -> ScriptBuf {
        get_timelocked_redeemscript_from_index(
            &Secp256k1::new(),
            &get_timelocked_master_key_from_root_master_key(&self.store.master_key),
            index,
        )
    }

    pub fn get_timelocked_privkey_from_index(&self, index: u32) -> SecretKey {
        get_timelocked_master_key_from_root_master_key(&self.store.master_key)
            .ckd_priv(&Secp256k1::new(), ChildNumber::Normal { index })
            .unwrap()
            .private_key
    }

    pub fn get_timelocked_address(&self, locktime: &YearAndMonth) -> (Address, i64) {
        let redeemscript = self.get_timelocked_redeemscript_from_index(locktime.to_index());
        let addr = Address::p2wsh(&redeemscript, self.store.network);
        let unix_locktime = read_locktime_from_timelocked_redeemscript(&redeemscript)
            .expect("bug: unable to read locktime");
        (addr, unix_locktime)
    }

    //returns Ok(None) if no fidelity bonds in wallet
    pub fn find_most_valuable_fidelity_bond(&self, rpc: &Client) -> Option<HotWalletFidelityBond> {
        let list_unspent_result = self.list_unspent_from_wallet(false, true).unwrap();
        let fidelity_bond_utxos = list_unspent_result
            .iter()
            .filter(|(utxo, _)| utxo.confirmations > 0)
            .filter(|(_, usi)| {
                matches!(
                    usi,
                    UTXOSpendInfo::FidelityBondCoin {
                        index: _,
                        input_value: _,
                    }
                )
            })
            .collect::<Vec<&(ListUnspentResultEntry, UTXOSpendInfo)>>();
        let fidelity_bond_values = fidelity_bond_utxos
            .iter()
            .map(|(utxo, usi)| calculate_timelocked_fidelity_bond_value_from_utxo(utxo, usi, rpc))
            .collect::<Vec<f64>>();
        fidelity_bond_utxos
            .iter()
            .zip(fidelity_bond_values.iter())
            //partial_cmp fails if NaN value involved, which wont happen, so unwrap() is acceptable
            .max_by(|(_, x), (_, y)| x.partial_cmp(y).unwrap())
            .map(|most_valuable_fidelity_bond| {
                HotWalletFidelityBond::new(
                    self,
                    &most_valuable_fidelity_bond.0 .0,
                    &most_valuable_fidelity_bond.0 .1,
                )
            })
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
                calculate_timelocked_fidelity_bond_value(
                    100000000,
                    (6.0 * YEAR) as i64,
                    0,
                    y * YEAR as u64,
                )
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
                calculate_timelocked_fidelity_bond_value(
                    100000000,
                    (6.0 * YEAR) as i64,
                    0,
                    (6 + y) * YEAR as u64,
                )
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
                calculate_timelocked_fidelity_bond_value(100000000, (y as f64 * YEAR) as i64, 0, 0)
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
                calculate_timelocked_fidelity_bond_value(
                    100000000,
                    ((200 + y) as f64 * YEAR) as i64,
                    0,
                    0,
                )
            })
            .collect::<Vec<f64>>();
        let value_diff = (0..values.len() - 1)
            .map(|i| values[i] - values[i + 1])
            .collect::<Vec<f64>>();
        for v in &value_diff {
            assert!(v.abs() < EPSILON);
        }
    }
}

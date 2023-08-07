use std::sync::Arc;

use std::convert::TryInto;

use bitcoin::{
    blockdata::{
        opcodes::{self, all},
        script::{Builder, Instruction, Script},
    },
    hashes::Hash,
    secp256k1,
    secp256k1::{
        rand::{rngs::OsRng, RngCore},
        Message, Secp256k1, SecretKey, Signature,
    },
    util::{bip143::SigHashCache, ecdsa::PublicKey},
    OutPoint, SigHashType, Transaction, TxIn, TxOut,
};

pub use bitcoin::hashes::hash160::Hash as Hash160;

use bitcoincore_rpc::{Client, RpcApi};

use crate::{error::TeleportError, protocol::messages::FundingTxInfo, wallet::Wallet};

//relatively simple handling of miner fees for now, each funding transaction is considered
// to have the same size, and taker will pay all the maker's miner fees based on that
//taker will choose what fee rate they will use, and how many funding transactions they want
// the makers to create
//this doesnt take into account the different sizes of single-sig, 2of2 multisig or htlc contracts
// but all those complications will go away when we move to ecdsa2p and scriptless scripts
// so theres no point adding complications for something that we'll hopefully get rid of soon
//this size here is for a tx with 2 p2wpkh outputs, 3 singlesig inputs and 1 2of2 multisig input
// if the maker can get stuff confirmed cheaper than this then they can keep that money
// if the maker ends up paying more then thats their problem
// we could avoid this guessing by adding one more round trip to the protocol where the maker
// calculates exactly how big the transactions will be and then taker knows exactly the miner fee
// to pay for
pub const MAKER_FUNDING_TX_VBYTE_SIZE: u64 = 372;

//like the Incoming/OutgoingSwapCoin structs but no privkey or signature information
//used by the taker to monitor coinswaps between two makers
#[derive(Debug, Clone)]
pub struct WatchOnlySwapCoin {
    pub sender_pubkey: PublicKey,
    pub receiver_pubkey: PublicKey,
    pub contract_tx: Transaction,
    pub contract_redeemscript: Script,
    pub funding_amount: u64,
}

// pub trait SwapCoin {
//     fn get_multisig_redeemscript(&self) -> Script;
//     fn get_contract_tx(&self) -> Transaction;
//     fn get_contract_redeemscript(&self) -> Script;
//     fn get_timelock_pubkey(&self) -> PublicKey;
//     fn get_timelock(&self) -> u16;
//     fn get_hashlock_pubkey(&self) -> PublicKey;
//     fn get_hashvalue(&self) -> Hash160;
//     fn get_funding_amount(&self) -> u64;
//     fn verify_contract_tx_receiver_sig(&self, sig: &Signature) -> bool;
//     fn verify_contract_tx_sender_sig(&self, sig: &Signature) -> bool;
//     fn apply_privkey(&mut self, privkey: SecretKey) -> Result<(), TeleportError>;
//     fn is_hash_preimage_known(&self) -> bool;
// }

pub fn calculate_coinswap_fee(
    absolute_fee_sat: u64,
    amount_relative_fee_ppb: u64,
    time_relative_fee_ppb: u64,
    total_funding_amount: u64,
    time_in_blocks: u64,
) -> u64 {
    absolute_fee_sat
        + (total_funding_amount * amount_relative_fee_ppb / 1_000_000_000)
        + (time_in_blocks * time_relative_fee_ppb / 1_000_000_000)
}

pub fn apply_two_signatures_to_2of2_multisig_spend(
    key1: &PublicKey,
    key2: &PublicKey,
    sig1: &Signature,
    sig2: &Signature,
    input: &mut TxIn,
    redeemscript: &Script,
) {
    let (sig_first, sig_second) = if key1.key.serialize()[..] < key2.key.serialize()[..] {
        (sig1, sig2)
    } else {
        (sig2, sig1)
    };

    input.witness.push(Vec::new()); //first is multisig dummy
    input.witness.push(sig_first.serialize_der().to_vec());
    input.witness.push(sig_second.serialize_der().to_vec());
    input.witness[1].push(SigHashType::All as u8);
    input.witness[2].push(SigHashType::All as u8);
    input.witness.push(redeemscript.to_bytes());
}

pub fn create_multisig_redeemscript(key1: &PublicKey, key2: &PublicKey) -> Script {
    let builder = Builder::new().push_opcode(all::OP_PUSHNUM_2);
    if key1.key.serialize()[..] < key2.key.serialize()[..] {
        builder.push_key(key1).push_key(key2)
    } else {
        builder.push_key(key2).push_key(key1)
    }
    .push_opcode(all::OP_PUSHNUM_2)
    .push_opcode(all::OP_CHECKMULTISIG)
    .into_script()
}

pub fn derive_maker_pubkey_and_nonce(
    tweakable_point: PublicKey,
) -> Result<(PublicKey, SecretKey), secp256k1::Error> {
    let mut nonce_bytes = [0u8; 32];
    let mut rng = OsRng::new().unwrap();
    rng.fill_bytes(&mut nonce_bytes);
    let nonce = SecretKey::from_slice(&nonce_bytes)?;
    let maker_pubkey = calculate_maker_pubkey_from_nonce(tweakable_point, nonce)?;

    Ok((maker_pubkey, nonce))
}

pub fn calculate_maker_pubkey_from_nonce(
    tweakable_point: PublicKey,
    nonce: SecretKey,
) -> Result<PublicKey, secp256k1::Error> {
    let secp = Secp256k1::new();

    let nonce_point = bitcoin::secp256k1::PublicKey::from_secret_key(&secp, &nonce);
    Ok(PublicKey {
        compressed: true,
        key: tweakable_point.key.combine(&nonce_point)?,
    })
}

pub fn find_funding_output<'a>(
    funding_tx: &'a Transaction,
    multisig_redeemscript: &Script,
) -> Option<(u32, &'a TxOut)> {
    let multisig_spk = redeemscript_to_scriptpubkey(&multisig_redeemscript);
    funding_tx
        .output
        .iter()
        .enumerate()
        .map(|(i, o)| (i as u32, o))
        .find(|(_i, o)| o.script_pubkey == multisig_spk)
}

/// Convert a redeemscript into p2wsh scriptpubkey.
pub fn redeemscript_to_scriptpubkey(redeemscript: &Script) -> Script {
    //p2wsh address
    Script::new_witness_program(
        bitcoin::bech32::u5::try_from_u8(0).unwrap(),
        &redeemscript.wscript_hash().to_vec(),
    )
}

#[rustfmt::skip]
pub fn create_contract_redeemscript(
    pub_hashlock: &PublicKey,
    pub_timelock: &PublicKey,
    hashvalue: Hash160,
    locktime: u16,
) -> Script {
    //avoid the malleability from OP_IF attack, see:
    //https://lists.linuxfoundation.org/pipermail/lightning-dev/2016-September/000605.html
    //the attack here is that OP_IF accepts anything nonzero as true, so someone
    // could replace the argument with something much bigger, which would
    // reduce the tx fee rate, the solution is to only use OP_IF after OP_EQUAL

    //avoid the oversize preimage attack
    //https://lists.linuxfoundation.org/pipermail/lightning-dev/2016-May/000529.html
    //one solution is adding `OP_SIZE 32 OP_EQUALVERIFY`
    // but then you force the locktime case to waste 32 bytes of witness
    //so we use this script which requires size zero for the locktime branch

    //we also want the hashlock case to be locked with 1 OP_CSV
    //which disables CPFP and therefore avoids transaction pinning
    //see https://bitcoinops.org/en/topics/transaction-pinning/

    /*
    opcodes                  | stack after execution
                             |
                             | <sig> <preimage>
    OP_SIZE                  | <sig> <preimage> <size>
    OP_SWAP                  | <sig> <size> <preimage>
    OP_HASH160               | <sig> <size> <hash>
    H(X)                     | <sig> <size> <hash> H(X)
    OP_EQUAL                 | <sig> <size> 1|0
    OP_IF                    |
        pub_hashlock         | <sig> <size> <pub>
        32                   | <sig> <size> <pub> 32
        1                    | <sig> <size> <pub> 32 1
    OP_ELSE                  |
        pub_timelock         | <sig> <size> <pub>
        0                    | <sig> <size> <pub> 0
        locktime             | <sig> <size> <pub> 0 <locktime>
    OP_ENDIF                 |
    OP_CHECKSEQUENCEVERIFY   | <sig> <size> <pub> (32|0) (1|<locktime>)
    OP_DROP                  | <sig> <size> <pub> (32|0)
    OP_ROT                   | <sig> <pub> (32|0) <size>
    OP_EQUALVERIFY           | <sig> <pub>
    OP_CHECKSIG              | true|false
    */

    //spent with witnesses:
    //hashlock case:
    //<hashlock_signature> <preimage len 32>
    //timelock case:
    //<timelock_signature> <empty_vector>

    Builder::new()
        .push_opcode(opcodes::all::OP_SIZE)
        .push_opcode(opcodes::all::OP_SWAP)
        .push_opcode(opcodes::all::OP_HASH160)
        .push_slice(&hashvalue[..])
        .push_opcode(opcodes::all::OP_EQUAL)
        .push_opcode(opcodes::all::OP_IF)
            .push_key(&pub_hashlock)
            .push_int(32)
            .push_int(1)
        .push_opcode(opcodes::all::OP_ELSE)
            .push_key(&pub_timelock)
            .push_int(0)
            .push_int(locktime as i64)
        .push_opcode(opcodes::all::OP_ENDIF)
        .push_opcode(opcodes::all::OP_CSV)
        .push_opcode(opcodes::all::OP_DROP)
        .push_opcode(opcodes::all::OP_ROT)
        .push_opcode(opcodes::all::OP_EQUALVERIFY)
        .push_opcode(opcodes::all::OP_CHECKSIG)
        .into_script()
}

//TODO put all these magic numbers in a const or something
//a better way is to use redeemscript.instructions() like read_locktime_from_contract()
pub fn read_hashvalue_from_contract(redeemscript: &Script) -> Result<Hash160, &'static str> {
    if redeemscript.to_bytes().len() < 25 {
        return Err("script too short");
    }
    Ok(Hash160::from_inner(
        redeemscript.to_bytes()[4..24]
            .try_into()
            .map_err(|_| "tryinto error")?,
    ))
}

pub fn read_locktime_from_contract(redeemscript: &Script) -> Option<u16> {
    match redeemscript.instructions().nth(12)?.ok()? {
        Instruction::PushBytes(locktime_bytes) => match locktime_bytes.len() {
            1 => Some(locktime_bytes[0] as u16),
            2 | 3 => {
                let (int_bytes, _rest) = locktime_bytes.split_at(std::mem::size_of::<u16>());
                Some(u16::from_le_bytes(int_bytes.try_into().unwrap()))
            }
            _ => None,
        },
        Instruction::Op(opcode) => {
            if let opcodes::Class::PushNum(n) = opcode.classify() {
                Some(n.try_into().ok()?)
            } else {
                None
            }
        }
    }
}

pub fn read_hashlock_pubkey_from_contract(
    redeemscript: &Script,
) -> Result<PublicKey, &'static str> {
    if redeemscript.to_bytes().len() < 61 {
        return Err("script too short");
    }
    PublicKey::from_slice(&redeemscript.to_bytes()[27..60]).map_err(|_| "pubkey error")
}

pub fn read_timelock_pubkey_from_contract(
    redeemscript: &Script,
) -> Result<PublicKey, &'static str> {
    if redeemscript.to_bytes().len() < 99 {
        return Err("script too short");
    }
    PublicKey::from_slice(&redeemscript.to_bytes()[65..98]).map_err(|_| "pubkey error")
}

pub fn read_pubkeys_from_multisig_redeemscript(
    redeemscript: &Script,
) -> Option<(PublicKey, PublicKey)> {
    let ms_rs_bytes = redeemscript.to_bytes();
    //TODO put these magic numbers in consts, PUBKEY1_OFFSET maybe
    let pubkey1 = PublicKey::from_slice(&ms_rs_bytes[2..35]);
    let pubkey2 = PublicKey::from_slice(&ms_rs_bytes[36..69]);
    if pubkey1.is_err() || pubkey2.is_err() {
        return None;
    }
    Some((pubkey1.unwrap(), pubkey2.unwrap()))
}

/// Create a Contract Transaction for the "Sender" side of Coinswap.
/// The Sender gets the coins back via timelock.
/// Receiver gets the coins via hashlock.
pub fn create_senders_contract_tx(
    input: OutPoint,
    input_value: u64,
    contract_redeemscript: &Script,
) -> Transaction {
    Transaction {
        input: vec![TxIn {
            previous_output: input,
            sequence: 0,
            witness: Vec::new(),
            script_sig: Script::new(),
        }],
        output: vec![TxOut {
            script_pubkey: redeemscript_to_scriptpubkey(&contract_redeemscript),
            // TODO: Mining fee for contract tx is hard coded here. Make it configurable.
            value: input_value - 1000,
        }],
        lock_time: 0,
        version: 2,
    }
}

pub fn create_receivers_contract_tx(
    input: OutPoint,
    input_value: u64,
    contract_redeemscript: &Script,
) -> Transaction {
    //exactly the same thing as senders contract for now, until collateral
    //inputs are implemented
    create_senders_contract_tx(input, input_value, contract_redeemscript)
}

fn is_contract_out_valid(
    contract_output: &TxOut,
    hashlock_pubkey: &PublicKey,
    timelock_pubkey: &PublicKey,
    hashvalue: Hash160,
    locktime: u16,
    minimum_locktime: u16,
) -> Result<(), TeleportError> {
    if minimum_locktime > locktime {
        return Err(TeleportError::Protocol("locktime too short"));
    }

    let redeemscript_from_request =
        create_contract_redeemscript(hashlock_pubkey, timelock_pubkey, hashvalue, locktime);
    let contract_spk_from_request = redeemscript_to_scriptpubkey(&redeemscript_from_request);
    if contract_output.script_pubkey != contract_spk_from_request {
        return Err(TeleportError::Protocol(
            "given transaction does not pay to requested contract",
        ));
    }
    Ok(())
}

//TODO perhaps rename this to include "_with_nonces"
//to match how "validate_and_sign_contract_tx" does it only with keys
pub fn validate_and_sign_senders_contract_tx(
    multisig_key_nonce: &SecretKey,
    hashlock_key_nonce: &SecretKey,
    timelock_pubkey: &PublicKey,
    senders_contract_tx: &Transaction,
    multisig_redeemscript: &Script,
    funding_input_value: u64,
    hashvalue: Hash160,
    locktime: u16,
    minimum_locktime: u16,
    tweakable_privkey: &SecretKey,
    wallet: &mut Wallet,
) -> Result<Signature, TeleportError> {
    if senders_contract_tx.input.len() != 1 || senders_contract_tx.output.len() != 1 {
        return Err(TeleportError::Protocol(
            "invalid number of inputs or outputs",
        ));
    }
    if !wallet.does_prevout_match_cached_contract(
        &senders_contract_tx.input[0].previous_output,
        &senders_contract_tx.output[0].script_pubkey,
    )? {
        return Err(TeleportError::Protocol(
            "taker attempting multiple contract attack, rejecting",
        ));
    }

    let secp = Secp256k1::new();
    let mut hashlock_privkey_from_nonce = *tweakable_privkey;
    hashlock_privkey_from_nonce
        .add_assign(hashlock_key_nonce.as_ref())
        .map_err(|_| {
            TeleportError::Protocol("error with hashlock tweakable privkey + hashlock nonce")
        })?;
    let hashlock_pubkey_from_nonce = PublicKey {
        compressed: true,
        key: secp256k1::PublicKey::from_secret_key(&secp, &hashlock_privkey_from_nonce),
    };

    is_contract_out_valid(
        &senders_contract_tx.output[0],
        &hashlock_pubkey_from_nonce,
        &timelock_pubkey,
        hashvalue,
        locktime,
        minimum_locktime,
    )?; //note question mark here propagating the error upwards

    wallet.cache_prevout_to_contract(
        senders_contract_tx.input[0].previous_output,
        senders_contract_tx.output[0].script_pubkey.clone(),
    )?;

    let mut multisig_privkey_from_nonce = *tweakable_privkey;
    multisig_privkey_from_nonce
        .add_assign(multisig_key_nonce.as_ref())
        .map_err(|_| {
            TeleportError::Protocol("error with multisig tweakable privkey + multisig nonce")
        })?;

    Ok(sign_contract_tx(
        &senders_contract_tx,
        &multisig_redeemscript,
        funding_input_value,
        &multisig_privkey_from_nonce,
    )
    .map_err(|_| TeleportError::Protocol("error with signing contract tx"))?)
}

//returns the keys of the multisig, ready for importing
//or None if the proof is invalid for some reason
//or an error if the RPC connection fails
pub fn verify_proof_of_funding(
    rpc: Arc<Client>,
    wallet: &mut Wallet,
    funding_info: &FundingTxInfo,
    funding_output_index: u32,
    next_locktime: u16,
    min_contract_react_time: u16,
    //returns my_multisig_privkey, other_multisig_pubkey, my_hashlock_privkey
) -> Result<(SecretKey, PublicKey, SecretKey), TeleportError> {
    //check the funding_tx exists and was really confirmed
    if let Some(txout) =
        rpc.get_tx_out(&funding_info.funding_tx.txid(), funding_output_index, None)?
    {
        if txout.confirmations < 1 {
            return Err(TeleportError::Protocol("funding tx not confirmed"));
        }
    } else {
        return Err(TeleportError::Protocol("funding tx output doesnt exist"));
    }

    //pattern match to check redeemscript is really a 2of2 multisig
    let mut ms_rs_bytes = funding_info.multisig_redeemscript.to_bytes();
    const PUB_PLACEHOLDER: [u8; 33] = [0x02; 33];
    let pubkey_placeholder = PublicKey::from_slice(&PUB_PLACEHOLDER).unwrap();
    let template_ms_rs =
        create_multisig_redeemscript(&pubkey_placeholder, &pubkey_placeholder).into_bytes();
    if ms_rs_bytes.len() != template_ms_rs.len() {
        return Err(TeleportError::Protocol(
            "wrong multisig_redeemscript length",
        ));
    }
    ms_rs_bytes.splice(2..35, PUB_PLACEHOLDER.iter().cloned());
    ms_rs_bytes.splice(36..69, PUB_PLACEHOLDER.iter().cloned());
    if ms_rs_bytes != template_ms_rs {
        return Err(TeleportError::Protocol(
            "multisig_redeemscript not matching template",
        ));
    }

    //check my pubkey is one of the pubkeys in the redeemscript
    let (pubkey1, pubkey2) =
        read_pubkeys_from_multisig_redeemscript(&funding_info.multisig_redeemscript)
            .ok_or(TeleportError::Protocol("invalid multisig_redeemscript"))?;
    let (tweakable_privkey, tweakable_point) = wallet.get_tweakable_keypair();
    let my_pubkey = calculate_maker_pubkey_from_nonce(tweakable_point, funding_info.multisig_nonce)
        .map_err(|_| TeleportError::Protocol("unable to calculate maker pubkey from nonce"))?;
    if pubkey1 != my_pubkey && pubkey2 != my_pubkey {
        return Err(TeleportError::Protocol(
            "wrong pubkeys in multisig_redeemscript",
        ));
    }

    //check that the new locktime is sufficently short enough compared to the
    //locktime in the provided funding tx
    let locktime = read_locktime_from_contract(&funding_info.contract_redeemscript).ok_or(
        TeleportError::Protocol("unable to read locktime from contract"),
    )?;
    //this is the time the maker or his watchtowers have to be online, read
    // the hash preimage from the blockchain and broadcast their own tx
    if locktime - next_locktime < min_contract_react_time {
        return Err(TeleportError::Protocol("locktime too short"));
    }

    //check that provided hashlock_key_nonce really corresponds to the hashlock_pubkey in contract
    let contract_hashlock_pubkey =
        read_hashlock_pubkey_from_contract(&funding_info.contract_redeemscript)
            .map_err(|_| TeleportError::Protocol("unable to read hashlock pubkey from contract"))?;
    let derived_hashlock_pubkey =
        calculate_maker_pubkey_from_nonce(tweakable_point, funding_info.hashlock_nonce)
            .map_err(|_| TeleportError::Protocol("unable to calculate maker pubkey from nonce"))?;
    if contract_hashlock_pubkey != derived_hashlock_pubkey {
        return Err(TeleportError::Protocol(
            "contract hashlock pubkey doesnt match key derived from nonce",
        ));
    }

    //check that the provided contract matches the scriptpubkey from the
    //cache which was populated when the signsendercontracttx message arrived
    let contract_spk = redeemscript_to_scriptpubkey(&funding_info.contract_redeemscript);

    if !wallet.does_prevout_match_cached_contract(
        &OutPoint {
            txid: funding_info.funding_tx.txid(),
            vout: funding_output_index,
        },
        &contract_spk,
    )? {
        return Err(TeleportError::Protocol(
            "provided contract does not match sender contract tx, rejecting",
        ));
    }

    let mut my_privkey = tweakable_privkey;
    my_privkey
        .add_assign(funding_info.multisig_nonce.as_ref())
        .map_err(|_| TeleportError::Protocol("error with wallet tweakable privkey + nonce"))?;
    let mut hashlock_privkey = tweakable_privkey;
    hashlock_privkey
        .add_assign(funding_info.hashlock_nonce.as_ref())
        .map_err(|_| TeleportError::Protocol("error with wallet tweakable privkey + nonce"))?;

    let other_pubkey = if pubkey1 == my_pubkey {
        pubkey2
    } else {
        pubkey1
    };
    Ok((my_privkey, other_pubkey, hashlock_privkey))
}

pub fn validate_contract_tx(
    receivers_contract_tx: &Transaction,
    funding_outpoint: Option<&OutPoint>,
    contract_redeemscript: &Script,
) -> Result<(), TeleportError> {
    if receivers_contract_tx.input.len() != 1 || receivers_contract_tx.output.len() != 1 {
        return Err(TeleportError::Protocol(
            "invalid number of inputs or outputs",
        ));
    }
    if funding_outpoint.is_some()
        && receivers_contract_tx.input[0].previous_output != *funding_outpoint.unwrap()
    {
        return Err(TeleportError::Protocol("not spending the funding outpoint"));
    }
    if receivers_contract_tx.output[0].script_pubkey
        != redeemscript_to_scriptpubkey(&contract_redeemscript)
    {
        return Err(TeleportError::Protocol("doesnt pay to requested contract"));
    }
    Ok(())
}

pub fn sign_contract_tx(
    contract_tx: &Transaction,
    multisig_redeemscript: &Script,
    funding_amount: u64,
    privkey: &SecretKey,
) -> Result<Signature, secp256k1::Error> {
    let input_index = 0;
    let sighash = Message::from_slice(
        &SigHashCache::new(contract_tx).signature_hash(
            input_index,
            multisig_redeemscript,
            funding_amount,
            SigHashType::All,
        )[..],
    )?;
    let secp = Secp256k1::new();
    Ok(secp.sign(&sighash, privkey))
}

pub fn verify_contract_tx_sig(
    contract_tx: &Transaction,
    multisig_redeemscript: &Script,
    funding_amount: u64,
    pubkey: &PublicKey,
    sig: &Signature,
) -> bool {
    let input_index = 0;
    let sighash = match Message::from_slice(
        &SigHashCache::new(contract_tx).signature_hash(
            input_index,
            multisig_redeemscript,
            funding_amount,
            SigHashType::All,
        )[..],
    ) {
        Ok(sig) => sig,
        Err(_) => return false,
    };
    let secp = Secp256k1::new();
    secp.verify(&sighash, sig, &pubkey.key).is_ok()
}

#[cfg(test)]
mod test {
    use super::*;
    use bitcoin::{
        consensus::encode::deserialize,
        hashes::hex::{FromHex, ToHex},
        secp256k1::rand::{random, thread_rng, Rng},
        PrivateKey,
    };
    use std::{str::FromStr, string::String};

    fn read_pubkeys_from_contract_reedimscript(
        contract_script: &Script,
    ) -> Result<(PublicKey, PublicKey), &'static str> {
        let script_bytes = contract_script.to_bytes();

        let hashpub =
            PublicKey::from_slice(&script_bytes[27..60]).map_err(|_| "Bad pubkey data")?;
        let timepub =
            PublicKey::from_slice(&script_bytes[65..98]).map_err(|_| "Bad pubkey data")?;

        Ok((hashpub, timepub))
    }

    #[test]
    fn test_maker_pubkey_computation() {
        let secp = Secp256k1::new();
        let sk =
            PrivateKey::from_wif("cVt4o7BGAig1UXywgGSmARhxMdzP5qvQsxKkSsc1XEkw3tDTQFpy").unwrap();
        let pubkey = sk.public_key(&secp);
        let nonce = SecretKey::from_slice(&[2; 32]).unwrap();
        let maker_key_computed = calculate_maker_pubkey_from_nonce(pubkey, nonce).unwrap();
        let expected_pubkey = PublicKey::from_str(
            "03bf98c86c3d536136378cf43ac42861ece609de87f5a44e19b730e8e9bd791938",
        )
        .unwrap();
        assert_eq!(expected_pubkey, maker_key_computed);
    }

    #[test]
    fn test_maker_pubkey_nonce_derviation() {
        let secp = Secp256k1::new();
        let privkey_org =
            PrivateKey::from_wif("cVt4o7BGAig1UXywgGSmARhxMdzP5qvQsxKkSsc1XEkw3tDTQFpy").unwrap();
        let pubkey_org = privkey_org.public_key(&secp);
        let (pubkey_derived, nonce) = derive_maker_pubkey_and_nonce(pubkey_org.clone()).unwrap();
        let nonce_point = secp256k1::PublicKey::from_secret_key(&secp, &nonce);
        let expected_derivation = PublicKey {
            compressed: true,
            key: pubkey_org.key.combine(&nonce_point).unwrap(),
        };
        assert_eq!(pubkey_derived, expected_derivation);
    }

    #[test]
    fn test_contract_script_generation() {
        // create a random hashvalue
        let hashvalue = Hash160::from_inner(thread_rng().gen::<[u8; 20]>());

        let pub_hashlock = PublicKey::from_str(
            "032e58afe51f9ed8ad3cc7897f634d881fdbe49a81564629ded8156bebd2ffd1af",
        )
        .unwrap();

        let pub_timelock = PublicKey::from_str(
            "039b6347398505f5ec93826dc61c19f47c66c0283ee9be980e29ce325a0f4679ef",
        )
        .unwrap();

        // Use an u16 to strictly positive 2 byte integer
        let locktime = random::<u16>();
        println!("randomly chosen locktime = {}", locktime);

        let contract_script =
            create_contract_redeemscript(&pub_hashlock, &pub_timelock, hashvalue, locktime);

        // Get the byte encoded locktime for script
        let locktime_bytecode = Builder::new().push_int(locktime as i64).into_script();

        // Below is hand made script string that should be expected
        let expected = "827ca914".to_owned()
            + &hashvalue.as_inner().to_hex()[..]
            + "876321"
            + &pub_hashlock.to_string()[..]
            + "0120516721"
            + &pub_timelock.to_string()[..]
            + "00"
            + &format!("{:x}", locktime_bytecode)
            + "68b2757b88ac";

        assert_eq!(&format!("{:x}", contract_script), &expected);

        // Check data extraction from script is also working
        assert_eq!(
            read_hashvalue_from_contract(&contract_script).unwrap(),
            hashvalue
        );
        assert_eq!(
            read_locktime_from_contract(&contract_script).unwrap(),
            locktime
        );
    }

    #[test]
    fn test_pubkey_extraction_from_2of2_multisig() {
        // Create pubkeys to contruct 2of2 multi
        let pub1 = PublicKey::from_str(
            "032e58afe51f9ed8ad3cc7897f634d881fdbe49a81564629ded8156bebd2ffd1af",
        )
        .unwrap();

        let pub2 = PublicKey::from_str(
            "039b6347398505f5ec93826dc61c19f47c66c0283ee9be980e29ce325a0f4679ef",
        )
        .unwrap();

        let multisig = create_multisig_redeemscript(&pub1, &pub2);

        // Check script generation works
        assert_eq!(format!("{:x}", multisig), "5221032e58afe51f9ed8ad3cc7897f634d881fdbe49a81564629ded8156bebd2ffd1af21039b6347398505f5ec93826dc61c19f47c66c0283ee9be980e29ce325a0f4679ef52ae");

        // Check pubkey fetching from the script works
        let (fetched_pub1, fetched_pub2) =
            read_pubkeys_from_multisig_redeemscript(&multisig).unwrap();

        assert_eq!(fetched_pub1, pub1);
        assert_eq!(fetched_pub2, pub2);
    }

    #[test]
    fn test_find_funding_output() {
        // Create a 20f2 multi + another random spk
        let multisig_reedemscript = Script::from(Vec::from_hex("5221032e58afe51f9ed8ad3cc7897f634d881fdbe49a81564629ded8156bebd2ffd1af21039b6347398505f5ec93826dc61c19f47c66c0283ee9be980e29ce325a0f4679ef52ae").unwrap());
        let another_script = Script::from(Vec::from_hex("020000000156944c5d3f98413ef45cf54545538103cc9f298e0575820ad3591376e2e0f65d2a0000000000000000014871000000000000220020dad1b452caf4a0f26aecf1cc43aaae9b903a043c34f75ad9a36c86317b22236800000000").unwrap());

        let multi_script_pubkey = redeemscript_to_scriptpubkey(&multisig_reedemscript);
        let another_script_pubkey = redeemscript_to_scriptpubkey(&another_script);

        // Create the funding transaction
        let funding_tx = Transaction {
            input: vec![TxIn {
                previous_output: OutPoint::from_str(
                    "5df6e0e2761359d30a8275058e299fcc0381534545f55cf43e41983f5d4c9456:42",
                )
                .unwrap(),
                sequence: 0,
                witness: Vec::new(),
                script_sig: Script::new(),
            }],
            output: vec![
                TxOut {
                    script_pubkey: another_script_pubkey,
                    value: 2000,
                },
                TxOut {
                    script_pubkey: multi_script_pubkey,
                    value: 3000,
                },
            ],
            lock_time: 0,
            version: 2,
        };

        // Check the correct 2of2 multisig output is extracted from funding tx
        assert_eq!(
            (1u32, &funding_tx.output[1]),
            find_funding_output(&funding_tx, &multisig_reedemscript).unwrap()
        );
    }

    #[test]
    fn test_contract_tx_miscellaneous() {
        let contract_script = Script::from(Vec::from_hex(
            "827ca91414cdf8fe0b7b2db2bd976f27fb6f3cd5f9228633876321038cc778b555c3fe2b01d1b550a07\
            d26e38c026c4c4e1dee2a41f0431283230ee0012051672102b6b9ab72d42fb625a24598a792fa5346aa\
            64d728b446f7560f4ce1c29378b22c00012868b2757b88ac").unwrap());

        // Contract transaction spending utxo, randomly choosen
        let spending_utxo = OutPoint::from_str(
            "5df6e0e2761359d30a8275058e299fcc0381534545f55cf43e41983f5d4c9456:42",
        )
        .unwrap();

        // Create a contract transaction spending the above utxo
        let contract_tx = create_receivers_contract_tx(spending_utxo, 30000, &contract_script);

        // Check creation matches expectation
        let expected_tx_hex = String::from(
            "020000000156944c5d3f98413ef45cf54545538103cc9f298e057\
            5820ad3591376e2e0f65d2a0000000000000000014871000000000000220020046134873fba03e9b2c961\
            1f814d323e0772ced538f04c242b7a833018d58f3500000000",
        );
        let expected_tx: Transaction =
            deserialize(&Vec::from_hex(&expected_tx_hex).unwrap()).unwrap();
        assert_eq!(expected_tx, contract_tx);

        // Extract contract script data
        let hashvalue = read_hashvalue_from_contract(&contract_script).unwrap();
        let locktime = read_locktime_from_contract(&contract_script).unwrap();
        let (pub1, pub2) = read_pubkeys_from_contract_reedimscript(&contract_script).unwrap();

        // Validates if contract outpoint is correct
        assert!(is_contract_out_valid(
            &contract_tx.output[0],
            &pub1,
            &pub2,
            hashvalue,
            locktime,
            2
        )
        .is_ok());

        // Validate if the contract transaction is spending correctl utxo
        assert!(validate_contract_tx(&contract_tx, Some(&spending_utxo), &contract_script).is_ok());

        // Error Cases---------------------------------------------
        // Check validation against wrong spending outpoint
        if let TeleportError::Protocol(message) = validate_contract_tx(
            &contract_tx,
            Some(
                &OutPoint::from_str(
                    "5df6e0e2761359d30a8275058e299fcc0381534545f55cf43e41983f5d4c9456:40",
                )
                .unwrap(),
            ),
            &contract_script,
        )
        .unwrap_err()
        {
            assert_eq!(message, "not spending the funding outpoint")
        } else {
            panic!();
        }

        // Push one more input in contract transaction
        let mut contract_tx_err1 = contract_tx.clone();
        contract_tx_err1.input.push(TxIn {
            previous_output: OutPoint::from_str(
                "5df6e0e2761359d30a8275058e299fcc0381534545f55cf43e41983f5d4c9456:42",
            )
            .unwrap(),
            sequence: 0,
            witness: Vec::new(),
            script_sig: Script::new(),
        });
        // Verify validation fails
        if let TeleportError::Protocol(message) =
            validate_contract_tx(&contract_tx_err1, Some(&spending_utxo), &contract_script)
                .unwrap_err()
        {
            assert_eq!(message, "invalid number of inputs or outputs");
        } else {
            panic!();
        }

        // Change contract transaction to pay into wrong output
        let mut contract_tx_err2 = contract_tx.clone();
        let multisig_redeemscript = Script::from(Vec::from_hex("5221032e58afe51f9ed8ad3cc7897f634d881fdbe49a81564629ded8156bebd2ffd1af21039b6347398505f5ec93826dc61c19f47c66c0283ee9be980e29ce325a0f4679ef52ae").unwrap());
        let multi_script_pubkey = redeemscript_to_scriptpubkey(&multisig_redeemscript);
        contract_tx_err2.output[0] = TxOut {
            script_pubkey: multi_script_pubkey,
            value: 3000,
        };
        // Verify validation fails
        if let TeleportError::Protocol(message) =
            validate_contract_tx(&contract_tx_err2, Some(&spending_utxo), &contract_script)
                .unwrap_err()
        {
            assert_eq!(message, "doesnt pay to requested contract");
        } else {
            panic!();
        }
    }

    #[test]
    fn test_contract_sig_validation() {
        // First create a funding transaction
        let secp = Secp256k1::new();
        let priv_1 =
            PrivateKey::from_wif("cVt4o7BGAig1UXywgGSmARhxMdzP5qvQsxKkSsc1XEkw3tDTQFpy").unwrap();
        let priv_2 =
            PrivateKey::from_wif("5JYkZjmN7PVMjJUfJWfRFwtuXTGB439XV6faajeHPAM9Z2PT2R3").unwrap();

        let pub1 = priv_1.public_key(&secp);
        let pub2 = priv_2.public_key(&secp);

        let funding_outpoint_script = create_multisig_redeemscript(&pub1, &pub2);

        let funding_spk = redeemscript_to_scriptpubkey(&funding_outpoint_script);

        let funding_tx = Transaction {
            input: vec![TxIn {
                // random outpoint
                previous_output: OutPoint::from_str(
                    "5df6e0e2761359d30a8275058e299fcc0381534545f55cf43e41983f5d4c9456:42",
                )
                .unwrap(),
                sequence: 0,
                witness: Vec::new(),
                script_sig: Script::new(),
            }],
            output: vec![TxOut {
                script_pubkey: funding_spk,
                value: 2000,
            }],
            lock_time: 0,
            version: 2,
        };

        // Create the contract transaction spending the funding outpoint
        let funding_outpoint = OutPoint::new(funding_tx.txid(), 0);

        let contract_script = Script::from(Vec::from_hex("827ca914cdccf6695323f22d061a58c398deba38bba47148876321032e58afe51f9ed8ad3cc7897f634d881fdbe49a81564629ded8156bebd2ffd1af0120516721039b6347398505f5ec93826dc61c19f47c66c0283ee9be980e29ce325a0f4679ef000812dabb690fe0fd3768b2757b88ac").unwrap());

        let contract_tx = create_receivers_contract_tx(
            funding_outpoint,
            funding_tx.output[0].value,
            &contract_script,
        );

        // priv1 signs the contract and verify
        let sig1 = sign_contract_tx(
            &contract_tx,
            &funding_outpoint_script,
            funding_tx.output[0].value,
            &priv_1.key,
        )
        .unwrap();

        assert_eq!(
            verify_contract_tx_sig(
                &contract_tx,
                &funding_outpoint_script,
                funding_tx.output[0].value,
                &pub1,
                &sig1
            ),
            true
        );

        // priv2 signs the contract and verify
        let sig2 = sign_contract_tx(
            &contract_tx,
            &funding_outpoint_script,
            funding_tx.output[0].value,
            &priv_2.key,
        )
        .unwrap();

        assert!(verify_contract_tx_sig(
            &contract_tx,
            &funding_outpoint_script,
            funding_tx.output[0].value,
            &pub2,
            &sig2
        ));
    }
}

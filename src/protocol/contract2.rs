//! This module implements the contract protocol for a 2-of-2 multisig transaction

use bitcoin::transaction::Version;
use secp256k1::musig::{self, PublicNonce, SecretNonce};
use super::error2::ProtocolError;
use bitcoin::{script, Amount, PublicKey, ScriptBuf, Sequence, Witness};
use bitcoin::locktime::absolute::LockTime;
use bitcoin::opcodes::all::{OP_CHECKSIG, OP_CLTV, OP_DROP, OP_EQUALVERIFY, OP_SHA256};
use super::musig2::{generate_new_nonce_pair, 
    generate_new_keypair,
    get_aggregated_nonce,
    generate_partial_signature, 
    sort_public_keys, 
    aggregate_public_keys,
    aggregate_partial_signatures,
    verify_partial_signature,
    verify_aggregated_signature
};
use bitcoin::secp256k1::{
    XOnlyPublicKey,
    rand::{rngs::OsRng, RngCore},
    SecretKey, Secp256k1
};
use bitcoin::blockdata::transaction::{TxIn, TxOut, Transaction, OutPoint};
use bitcoin::taproot::{self, LeafVersion, TapLeaf, TaprootBuilder, TapLeafHash};

// create_hashlock_script
fn create_hashlock_script(hash: &[u8; 32], pubkey: &XOnlyPublicKey) -> ScriptBuf {
    script::Builder::new()
        .push_opcode(OP_SHA256)
        .push_slice(hash)
        .push_opcode(OP_EQUALVERIFY)
        .push_x_only_key(pubkey)
        .push_opcode(OP_CHECKSIG)
        .into_script()
}
// create_timelock_script
fn create_timelock_script(locktime: LockTime, pubkey: &XOnlyPublicKey) -> ScriptBuf {
    script::Builder::new()
        .push_lock_time(locktime)
        .push_opcode(OP_CLTV)
        .push_opcode(OP_DROP)
        .push_x_only_key(pubkey)
        .push_opcode(OP_CHECKSIG)
        .into_script()
}

fn derive_maker_pubkey_and_salt(
    tweakable_point: &PublicKey,
) -> Result<(PublicKey, SecretKey), ProtocolError> {
    let mut salt_bytes = [0u8; 32];
    OsRng.fill_bytes(&mut salt_bytes);
    let salt= SecretKey::from_slice(&salt_bytes)?;
    let maker_pubkey = calculate_pubkey_from_salt(tweakable_point, &salt)?;
    Ok((maker_pubkey, salt))
}

fn calculate_pubkey_from_salt(
    tweakable_point: &PublicKey,
    salt: &SecretKey,
) -> Result<PublicKey, ProtocolError> {
    let secp = Secp256k1::new();

    let salt_point = bitcoin::secp256k1::PublicKey::from_secret_key(&secp, salt);
    Ok(PublicKey {
        compressed: true,
        inner: tweakable_point.inner.combine(&salt_point)?,
    })
}

fn create_taproot_script(
    hashlock_script: ScriptBuf,
    timelock_script: ScriptBuf,
    sender_pubkey: &secp256k1::PublicKey,
    receiver_pubkey: &secp256k1::PublicKey,
) -> ScriptBuf {
    let secp = Secp256k1::new();
    let aggregated_pubkey = get_aggregated_pubkey(sender_pubkey, &receiver_pubkey);
    let taproot_spendinfo = TaprootBuilder::new()
            .add_leaf(1, hashlock_script).unwrap()
            .add_leaf(1, timelock_script).unwrap()
            .finalize(&secp, aggregated_pubkey)
            .expect("taproot info finalized");
    ScriptBuf::new_p2tr(&secp, taproot_spendinfo.internal_key(), taproot_spendinfo.merkle_root())
}

fn generate_partial_pubkey() {
    unimplemented!()
}

// 1. Select UTXO(s)
// 2. Get change address
// 3. Get Script Pubkey and amount
fn create_contract_transaction(
    input: OutPoint,
    input_value: Amount,
    scriptPubKey: ScriptBuf,
    fee_rate: Amount
) -> Result<Transaction, ProtocolError> {
    Ok(Transaction {
        input: vec![TxIn {
            previous_output: input,
            script_sig: ScriptBuf::new(),
            sequence: Sequence::ZERO,
            witness: Witness::new(), 
        }],
        output: vec![TxOut {
            script_pubkey: scriptPubKey,
            value: input_value - fee_rate,
        }],
        lock_time: LockTime::ZERO,
        version: Version::TWO
    })
}

fn get_aggregated_pubkey(sender_pubkey: &secp256k1::PublicKey, receiver_pubkey: &&secp256k1::PublicKey) -> bitcoin::XOnlyPublicKey {
    let public_keys: Vec<&secp256k1::PublicKey> = vec![sender_pubkey, receiver_pubkey];
    aggregate_public_keys(&public_keys)
}

fn generate_nonce_pair(
    musig_key_agg_cache: &musig::KeyAggCache,
    pubkey: secp256k1::PublicKey,
    msg: secp256k1::Message,
    extra_rand: Option<[u8; 32]>,
) -> (SecretNonce, PublicNonce) {
    generate_new_nonce_pair(musig_key_agg_cache, pubkey, msg, extra_rand)
}

fn redeem_hashlock_path() {
    unimplemented!()
}

fn redeem_timelock_path() {
    unimplemented!()
}

fn verify_transaction_data() {
    unimplemented!()
}
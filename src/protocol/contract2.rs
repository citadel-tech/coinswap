//! This module implements the contract protocol for a 2-of-2 multisig transaction

use bitcoin::transaction::Version;
use super::error2::ProtocolError;
use bitcoin::key::rand;
use bitcoin::{script, Script, Amount, PublicKey, ScriptBuf, Sequence, Witness};
use bitcoin::secp256k1::{Keypair};
use bitcoin::EcdsaSighashType;
use bitcoin::sighash::SighashCache;
use bitcoin::locktime::absolute::LockTime;
use bitcoin::opcodes::all::{OP_CHECKSIG, OP_CLTV, OP_DROP, OP_EQUALVERIFY, OP_SHA256};
use bitcoin::secp256k1::{
    XOnlyPublicKey,
    rand::{rngs::OsRng, RngCore},
    SecretKey, Secp256k1,
    Message
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
    internal_pubkey: XOnlyPublicKey,
) -> ScriptBuf {
    let secp = Secp256k1::new();
    let taproot_spendinfo = TaprootBuilder::new()
            .add_leaf(1, hashlock_script.clone()).unwrap()
            .add_leaf(1, timelock_script.clone()).unwrap()
            .finalize(&secp, internal_pubkey)
            .expect("taproot info finalized");
    println!("Taproot spend info: {:?}", taproot_spendinfo);
    let hashlock_control_block = taproot_spendinfo.control_block(&(hashlock_script, LeafVersion::TapScript));
    println!("Hashlock control block: {:?}", hashlock_control_block.as_slice());
    let timelock_control_block = taproot_spendinfo.control_block(&(timelock_script, LeafVersion::TapScript));
    println!("Timelock control block: {:?}", timelock_control_block.as_slice());
    ScriptBuf::new_p2tr(&secp, taproot_spendinfo.internal_key(), taproot_spendinfo.merkle_root())
}

fn generate_partial_pubkey() {
    unimplemented!()
}

// 1. Select UTXO(s)
// 2. Get change address
// 3. Get Script Pubkey and amount
fn create_unsigned_contract_tx(
    input: OutPoint,
    input_value: Amount,
    script_pubkey: ScriptBuf,
    fee: Amount
) -> Result<Transaction, ProtocolError> {
    Ok(Transaction {
        input: vec![TxIn {
            previous_output: input,
            script_sig: ScriptBuf::new(),
            sequence: Sequence::ZERO,
            witness: Witness::new(),
        }],
        output: vec![TxOut {
            script_pubkey: script_pubkey,
            value: input_value - fee,
        }],
        lock_time: LockTime::ZERO,
        version: Version::TWO
    })
}

fn create_hashlock_control_block() {
    unimplemented!()
}

fn create_timelock_control_block() {
    unimplemented!()
}

fn verify_partial_signature() {
    unimplemented!()
}

fn verify_hashlock_path(privkey: &SecretKey, hashpreimage: &[u8; 32]) {
    unimplemented!()
}

fn verify_timelock_path(privkey: &SecretKey, locktime: LockTime) {
    unimplemented!()
}

fn verify_transaction_data() {
    unimplemented!()
}

fn sign_contract(privkey: SecretKey, tx: Transaction) {

}
#[cfg(test)] 
mod tests{

    use bitcoin::hex::FromHex;
    use bitcoin::key::{Parity, TapTweak};
    use bitcoin::sighash::Prevouts;
    use bitcoin::taproot::{ControlBlock, TaprootMerkleBranch};
    use bitcoin::{locktime, Address, TapNodeHash, TapSighashType};
    use bitcoind::bitcoincore_rpc::RawTx;
    use bitcoind::bitcoincore_rpc::{Auth::UserPass, Client, RpcApi,};
    use bitcoind::bitcoincore_rpc::bitcoincore_rpc_json::AddressType;
    use std::str::FromStr;
    use super::*;
    use bitcoin::{bech32::{self, hrp, Bech32m}, ecdsa::Signature, hashes::sha256d::Hash as Sha256d, transaction, Txid};

    // #[test]
    fn test_create_hashlock_script() {
        let hash = [0u8; 32];
        let secp = bitcoin::secp256k1::Secp256k1::new();
        let keypair = Keypair::new(&secp, &mut rand::thread_rng());
        let (pubkey, _) = keypair.x_only_public_key();
        println!("Pubkey: {:?}", pubkey);
        let script = create_hashlock_script(&hash, &pubkey);
        println!("Hashlock script: {:?}", script);
    }

    // #[test]
    fn test_create_timelock_script() {
        let locktime: LockTime = LockTime::from_height(1000u32).unwrap();
        let secp = bitcoin::secp256k1::Secp256k1::new();
        let keypair = Keypair::new(&secp, &mut rand::thread_rng());
        let (pubkey, _) = keypair.x_only_public_key();
        println!("Pubkey: {:?}", pubkey);
        let script = create_timelock_script(locktime, &pubkey);
        println!("Timelock script: {:?}", script);
    }

    // #[test]
    fn test_create_taproot_script() {
        let secp = bitcoin::secp256k1::Secp256k1::new();
        let keypair = Keypair::new(&secp, &mut rand::thread_rng());
        let (pubkey, _) = keypair.x_only_public_key();
        let hashlock_script = create_hashlock_script(&[0u8; 32], &pubkey);
        println!("Hashlock script: {:?}", hashlock_script);
        let timelock_script = create_timelock_script(LockTime::from_height(300).unwrap(), &pubkey);
        println!("Timelock script: {:?}", timelock_script);
        println!("aggregated_pubkey: {:?}", pubkey);
        let taproot_script = create_taproot_script(hashlock_script, timelock_script, pubkey);
        println!("Taproot script ASM {:?}", taproot_script.to_asm_string());
        println!("Taproot script hex {:?}", taproot_script.to_hex_string());
        println!("Taproot script bech32 {:?}", bech32::encode::<Bech32m>(hrp::BCRT, taproot_script.as_bytes()));
    }

    // #[test]
    fn test_create_taproot_transaction() {
        let input = OutPoint {
            txid: Txid::from_raw_hash(Sha256d::from_str("b175d4cf487d40021300788eecf3588b0d3462d0cac656f8ae961da6f497c2ed").unwrap()),
            vout: 0,
        };
        let input_value = Amount::from_sat(100000);
        let fee = Amount::from_sat(100);
        let script_pubkey = ScriptBuf::from_hex("51209d5d1eab7c785543561d1236d888f4ce4eb858c8ccea669bdba14e9244bf46c3").unwrap();
        let tx = create_unsigned_contract_tx(input, input_value, script_pubkey.clone(), fee).unwrap();
        println!("Script pubkey: {:?}", script_pubkey);
        println!("Transaction: {:?}", tx);
    }

    // #[test]
    fn create_and_fund_address() {
        // Create a new address using bitcoin core rpc
        let client = Client::new("127.0.0.1:21443/wallet/test_wallet", UserPass("user".to_string(), "pass".to_string())).unwrap();
        let blockchian_info = client.get_blockchain_info().unwrap();
        // println!("Blockchain info: {:?}", blockchian_info);
        let wallets = client.list_wallets().unwrap();
        // println!("Wallets: {:?}", wallets);
        let wallet_name = "test_wallet";
        if !wallets.contains(&wallet_name.to_string()) {
            client.create_wallet(wallet_name, None, None, None, None).unwrap();
        }
        client.load_wallet(wallet_name);
        // let address = client.get_new_address(None, None).unwrap();
        let address = Address::from_str("bcrt1qp2h29s8r0fk3cweeklrutd362j82szshwhacrw").unwrap();
        // println!("New address: {:?}", address);
        let checked_address = address.require_network(bitcoin::Network::Regtest).unwrap();
        // let address_info = client.get_address_info(&checked_address).unwrap();
        // println!("Address info: {:?}", address_info);
        let balance = client.get_balances().unwrap();
        println!("Balance: {:?}", balance);
        let utxos = client.list_unspent(None, None, Some(&[&checked_address]), None, None).unwrap();
        println!("UTXOs: {:?}", utxos);
    }

    use bitcoin::hashes::Hash;
    use bitcoin::hashes::sha256 as Sha256;
    // #[test]
    fn end_to_end_test() {
        let secp = Secp256k1::new();
        // let internal_key = Keypair::new(&secp, &mut rand::thread_rng());
        let internal_key_slice = [120, 19, 235, 46, 81, 164, 156, 83, 91, 100, 122, 65, 216, 217, 120, 84, 52, 218, 149, 113, 10, 70, 52, 200, 211, 201, 145, 202, 199, 129, 8, 220];
        let internal_key = Keypair::from_seckey_slice(&secp, &internal_key_slice).unwrap();
        println!("Internal key: {:?}", internal_key.secret_key());
        let (pubkey, _) = internal_key.x_only_public_key();
        let a_keypair_slice = [57, 36, 177, 212, 31, 75, 221, 50, 13, 55, 102, 155, 21, 64, 146, 106, 101, 189, 1, 167, 80, 10, 246, 136, 96, 80, 57, 67, 63, 194, 67, 241];
        let a_keypair = Keypair::from_seckey_slice(&secp, &a_keypair_slice).unwrap();
        println!("A keypair: {:?}", a_keypair.secret_key());
        let (a_pubkey, _) = a_keypair.x_only_public_key();
        let b_keypair_slice = [196, 70, 74, 243, 36, 44, 31, 223, 8, 77, 47, 179, 229, 217, 187, 66, 144, 87, 50, 11, 184, 136, 255, 92, 97, 154, 183, 102, 27, 126, 54, 140];
        let b_keypair = Keypair::from_seckey_slice(&secp, &b_keypair_slice).unwrap();
        println!("B keypair: {:?}", b_keypair.secret_key());
        let (b_pubkey, _) = b_keypair.x_only_public_key();
        let hash_preimage = [0u8; 32];
        println!("Hash preimage: {:?}", hash_preimage);
        let hash =  Sha256::Hash::hash(&hash_preimage);
        println!("Hash: {:?}", hash);
        let hashlock_script = create_hashlock_script(&hash.as_byte_array(), &b_pubkey);
        println!("Hashlock script: {:?}", hashlock_script);
        let timelock_script = create_timelock_script(LockTime::from_height(1000).unwrap(), &a_pubkey);
        println!("Timelock script: {:?}", timelock_script);
        let taproot_script = create_taproot_script(hashlock_script, timelock_script, pubkey);
        println!("Taproot script ASM {:?}", taproot_script.to_asm_string());
        let transaction = create_unsigned_contract_tx(
            OutPoint {
                txid: Txid::from_raw_hash(Sha256d::from_str("b2ed68f77f5a2ce9daa5b3569f4050293112187a53b4c7b604420440f77d9b2e").unwrap()),
                vout: 1,
            },
            Amount::from_sat(10000000),
            taproot_script.clone(),
            Amount::from_sat(1000)
        ).unwrap();
        sign_seed_transaction(&transaction);
        println!("Transaction: {:?}", transaction);
    }

    fn sign_seed_transaction(tx: &Transaction) {
        let client = Client::new("127.0.0.1:21443/wallet/test_wallet", UserPass("user".to_string(), "pass".to_string())).unwrap();
        let signed_tx = client.sign_raw_transaction_with_wallet(tx, None, None).unwrap();
        println!("Signed transaction: {:?}", signed_tx);
        let txid = client.send_raw_transaction(&signed_tx.hex).unwrap();
        println!("Transaction ID: {:?}", txid);
        client.generate_to_address(1, &Address::from_str("bcrt1qdcm6dj28v9jm9f2ykqkkntwe5khjtvdf8ldu6d").unwrap().require_network(bitcoin::Network::Regtest).unwrap());
    }

    #[test]
    fn sweep_transaction() {
        let secp = Secp256k1::new();
        let outpoint = OutPoint {
            txid: Txid::from_str("7d3dcad78b89904d524880f12b456a92c7ce7b649ec162971a2fa80f1f9a9fd0").unwrap(),
            vout: 0
        };
        let internal_key_slice = [120, 19, 235, 46, 81, 164, 156, 83, 91, 100, 122, 65, 216, 217, 120, 84, 52, 218, 149, 113, 10, 70, 52, 200, 211, 201, 145, 202, 199, 129, 8, 220];
        let internal_key = Keypair::from_seckey_slice(&secp, &internal_key_slice).unwrap();
        let x_only_internal_key = internal_key.x_only_public_key().0;
        let merkle_root = TapNodeHash::from_str("b113d5c2abc6184c19df6e5993a775a37f2d62a29ef7145cf594f799818a0c20").unwrap();
        // sweep_via_internal_key(outpoint, internal_key, Some(merkle_root));

        let hash_preimage = [0u8; 32];
        let hashlock_key_slice = [196, 70, 74, 243, 36, 44, 31, 223, 8, 77, 47, 179, 229, 217, 187, 66, 144, 87, 50, 11, 184, 136, 255, 92, 97, 154, 183, 102, 27, 126, 54, 140];
        let hashlock_keypair = Keypair::from_seckey_slice(&secp, &hashlock_key_slice).unwrap();
        let hashlock_controlblock = ControlBlock { 
            leaf_version: LeafVersion::TapScript,
            output_key_parity: Parity::Odd,
            internal_key: x_only_internal_key,
            merkle_branch: TaprootMerkleBranch::decode(Vec::<u8>::from_hex("b6339ae2ac74053ee75ad9eae15d6829d68b31efea128cc8a0dd978a20ca0a60").unwrap().as_slice()).unwrap(),
        };
        // sweep_via_hashlock(outpoint, hash_preimage, hashlock_keypair, hashlock_controlblock);

        let timelock = LockTime::from_height(1000).unwrap();
        let timelock_key_slice = [57, 36, 177, 212, 31, 75, 221, 50, 13, 55, 102, 155, 21, 64, 146, 106, 101, 189, 1, 167, 80, 10, 246, 136, 96, 80, 57, 67, 63, 194, 67, 241];
        let timelock_keypair = Keypair::from_seckey_slice(&secp, &timelock_key_slice).unwrap();
        let timelock_controlblock = ControlBlock { 
            leaf_version: LeafVersion::TapScript,
            output_key_parity: Parity::Odd,
            internal_key: x_only_internal_key,
            merkle_branch: TaprootMerkleBranch::decode(Vec::<u8>::from_hex("da44244f79ba55530f671b2c695d0476b3b8260cb2cc5e04e0fc92a52a34eed8").unwrap().as_slice()).unwrap(),
        };
        sweep_via_timelock(outpoint, timelock, timelock_keypair, timelock_controlblock);
    }

    fn sweep_via_internal_key(outpoint: OutPoint, internal_keypair: Keypair, merkle_root: Option<TapNodeHash>) {
        let secp = Secp256k1::new();
        let client = Client::new("127.0.0.1:21443/wallet/test_wallet", UserPass("user".to_string(), "pass".to_string())).unwrap();
        let output_address = Address::from_str("bcrt1qdcm6dj28v9jm9f2ykqkkntwe5khjtvdf8ldu6d").unwrap().require_network(bitcoin::Network::Regtest).unwrap();
        let txin = TxIn {
            previous_output: outpoint,
            script_sig: ScriptBuf::new(),
            sequence: Sequence::ZERO,
            witness: Witness::default(),
        };
        let prevout = client.get_tx_out(&Txid::from_str("2a6302a7e7a01d6e7b27b7a9874287ab996af5fb30cf609f1e9c944d6341d2d5").unwrap(), 0, None).unwrap().unwrap();
        let prev_txout = TxOut {
            script_pubkey: ScriptBuf::from_bytes(prevout.script_pub_key.hex),
            value: Amount::from_sat(9999000),
        };
        let output = TxOut {
            script_pubkey: output_address.script_pubkey(),
            value: Amount::from_sat(9998000),
        };
        let mut unsigned_transaction = Transaction {
            input: vec![txin],
            output: vec![output.clone()],
            lock_time: LockTime::ZERO,
            version: Version::TWO,
        };

        let sighash_type = TapSighashType::Default;
        let prevouts = vec![prev_txout];
        let prevouts = Prevouts::All(&prevouts);

        let mut sighasher = SighashCache::new(&mut unsigned_transaction);
        let sighash = sighasher.taproot_key_spend_signature_hash(0, &prevouts, sighash_type);
        let tweaked = internal_keypair.tap_tweak(&secp, merkle_root);
        let sighash = sighash.expect("Failed to compute sighash");
        let msg = Message::from(sighash);
        let signature = secp.sign_schnorr(&msg, &tweaked.to_inner());

        let signature = bitcoin::taproot::Signature { signature, sighash_type};
        *sighasher.witness_mut(0).unwrap() = Witness::p2tr_key_spend(&signature);

        let tx = sighasher.into_transaction();
        println!("{:#?}", tx);
        let txid = client.send_raw_transaction(tx.raw_hex());
        println!("Transaction ID: {:?}", txid);
    }

    fn sweep_via_hashlock(outpoint: OutPoint, hash_preimage: [u8; 32], hashlock_keypair: Keypair, control_block: ControlBlock) {
        let secp = Secp256k1::new();
        let client = Client::new("127.0.0.1:21443/wallet/test_wallet", UserPass("user".to_string(), "pass".to_string())).unwrap();
        let output_address = Address::from_str("bcrt1qdcm6dj28v9jm9f2ykqkkntwe5khjtvdf8ldu6d").unwrap().require_network(bitcoin::Network::Regtest).unwrap();
        let txin = TxIn {
            previous_output: outpoint,
            script_sig: ScriptBuf::new(),
            sequence: Sequence::ZERO,
            witness: Witness::default(),
        };
        let prevout = client.get_tx_out(&Txid::from_str("4e46e549e4ba752851f2f425b24c4be2266db620d4217cf6ca548c6820c893ba").unwrap(), 0, None).unwrap().unwrap();
        let prev_txout = TxOut {
            script_pubkey: ScriptBuf::from_bytes(prevout.script_pub_key.hex),
            value: Amount::from_sat(9999000),
        };
        let output = TxOut {
            script_pubkey: output_address.script_pubkey(),
            value: Amount::from_sat(9998000),
        };
        let mut unsigned_transaction = Transaction {
            input: vec![txin],
            output: vec![output.clone()],
            lock_time: LockTime::ZERO,
            version: Version::TWO,
        };

        let (x_only_pubkey, _) = hashlock_keypair.x_only_public_key();
        let hash =  Sha256::Hash::hash(&hash_preimage);
        let hashlock_script = create_hashlock_script(&hash.as_byte_array(), &x_only_pubkey);

        let sighash_type = TapSighashType::All;
        let prevouts = vec![prev_txout];
        let prevouts = Prevouts::All(&prevouts);
        let mut sighasher = SighashCache::new(&mut unsigned_transaction);
        let sighash = sighasher.taproot_script_spend_signature_hash(0, &prevouts, TapLeafHash::from_script(&hashlock_script, LeafVersion::TapScript), sighash_type).expect("Failed to compute sighash");
        let msg = Message::from(sighash);
        let signature = secp.sign_schnorr(&msg, &hashlock_keypair);

        let signature = bitcoin::taproot::Signature { signature, sighash_type};
        let mut witness = Witness::new();
        witness.push(signature.to_vec());
        witness.push(hash_preimage.to_vec());
        
        witness.push(hashlock_script.as_bytes());
        witness.push(control_block.serialize());

        *sighasher.witness_mut(0).unwrap() = witness;
        let tx = sighasher.into_transaction();
        println!("{:#?}", tx);
        let txid = client.send_raw_transaction(tx.raw_hex());
        println!("Transaction ID: {:?}", txid);
    }

    fn sweep_via_timelock(outpoint: OutPoint, timelock: LockTime, timelock_keypair: Keypair, control_block: ControlBlock) {
        let secp = Secp256k1::new();
        let client = Client::new("127.0.0.1:21443/wallet/test_wallet", UserPass("user".to_string(), "pass".to_string())).unwrap();
        let output_address = Address::from_str("bcrt1qdcm6dj28v9jm9f2ykqkkntwe5khjtvdf8ldu6d").unwrap().require_network(bitcoin::Network::Regtest).unwrap();
        let txin = TxIn {
            previous_output: outpoint,
            script_sig: ScriptBuf::new(),
            sequence: Sequence::ZERO,
            witness: Witness::default(),
        };
        let prevout = client.get_tx_out(&Txid::from_str("7d3dcad78b89904d524880f12b456a92c7ce7b649ec162971a2fa80f1f9a9fd0").unwrap(), 0, None).unwrap().unwrap();
        let prev_txout = TxOut {
            script_pubkey: ScriptBuf::from_bytes(prevout.script_pub_key.hex),
            value: Amount::from_sat(9999000),
        };
        let output = TxOut {
            script_pubkey: output_address.script_pubkey(),
            value: Amount::from_sat(9998000),
        };
        let mut unsigned_transaction = Transaction {
            input: vec![txin],
            output: vec![output.clone()],
            lock_time: timelock,
            version: Version::TWO,
        };

        let (x_only_pubkey, _) = timelock_keypair.x_only_public_key();
        let timelock_script = create_timelock_script(timelock, &x_only_pubkey); 

        let sighash_type = TapSighashType::All;
        let prevouts = vec![prev_txout];
        let prevouts_all = Prevouts::All(&prevouts);
        let mut sighasher = SighashCache::new(&mut unsigned_transaction);
        let script_leaf_hash = TapLeafHash::from_script(&timelock_script, LeafVersion::TapScript);
        let sighash = sighasher.taproot_script_spend_signature_hash(0, &prevouts_all, script_leaf_hash, sighash_type)
            .expect("Failed to compute sighash for timelock script spend");

        let msg = Message::from(sighash);
        let signature = secp.sign_schnorr(&msg, &timelock_keypair);

        let taproot_signature = bitcoin::taproot::Signature { signature, sighash_type};
        let mut witness = Witness::new();
        witness.push(taproot_signature.to_vec());
        witness.push(timelock_script.as_bytes()); 
        witness.push(control_block.serialize()); 

        *sighasher.witness_mut(0).unwrap() = witness;

        let tx = sighasher.into_transaction();
        println!("Timelock Spend Tx Hex: {}", tx.raw_hex());
        println!("{:#?}", tx);
        let txid = client.send_raw_transaction(tx.raw_hex());
        println!("Transaction ID: {:?}", txid);
    }
}
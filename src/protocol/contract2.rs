//! This module implements the contract protocol for a 2-of-2 multisig transaction

use super::error::ProtocolError;
use bitcoin::{
    blockdata::transaction::{Transaction, TxOut},
    locktime::absolute::LockTime,
    opcodes::all::{OP_CHECKSIG, OP_CLTV, OP_DROP, OP_EQUALVERIFY, OP_SHA256},
    script,
    secp256k1::{Secp256k1, XOnlyPublicKey},
    sighash::{Prevouts, SighashCache},
    taproot::{LeafVersion, TaprootBuilder, TaprootSpendInfo},
    Amount, ScriptBuf,
};

// create_hashlock_script
pub(crate) fn create_hashlock_script(hash: &[u8; 32], pubkey: &XOnlyPublicKey) -> ScriptBuf {
    script::Builder::new()
        .push_opcode(OP_SHA256)
        .push_slice(hash)
        .push_opcode(OP_EQUALVERIFY)
        .push_x_only_key(pubkey)
        .push_opcode(OP_CHECKSIG)
        .into_script()
}
// create_timelock_script
pub(crate) fn create_timelock_script(locktime: LockTime, pubkey: &XOnlyPublicKey) -> ScriptBuf {
    script::Builder::new()
        .push_lock_time(locktime)
        .push_opcode(OP_CLTV)
        .push_opcode(OP_DROP)
        .push_x_only_key(pubkey)
        .push_opcode(OP_CHECKSIG)
        .into_script()
}

pub(crate) fn create_taproot_script(
    hashlock_script: ScriptBuf,
    timelock_script: ScriptBuf,
    internal_pubkey: XOnlyPublicKey,
) -> (ScriptBuf, TaprootSpendInfo) {
    let secp = Secp256k1::new();
    let taproot_spendinfo = TaprootBuilder::new()
        .add_leaf(1, hashlock_script.clone())
        .unwrap()
        .add_leaf(1, timelock_script.clone())
        .unwrap()
        .finalize(&secp, internal_pubkey)
        .expect("taproot info finalized");
    let _hashlock_control_block =
        taproot_spendinfo.control_block(&(hashlock_script, LeafVersion::TapScript));
    let _timelock_control_block =
        taproot_spendinfo.control_block(&(timelock_script, LeafVersion::TapScript));
    (
        ScriptBuf::new_p2tr(
            &secp,
            taproot_spendinfo.internal_key(),
            taproot_spendinfo.merkle_root(),
        ),
        taproot_spendinfo,
    )
}

/// Calculate Taproot sighash for contract spending
pub(crate) fn calculate_contract_sighash(
    spending_tx: &Transaction,
    contract_amount: Amount,
    hashlock_script: &ScriptBuf,
    timelock_script: &ScriptBuf,
    internal_key: bitcoin::secp256k1::XOnlyPublicKey,
) -> Result<bitcoin::secp256k1::Message, ProtocolError> {
    // Reconstruct the Taproot script
    let (contract_script, _) = create_taproot_script(
        hashlock_script.clone(),
        timelock_script.clone(),
        internal_key,
    );

    // Create prevout for sighash calculation
    let prevout = TxOut {
        value: contract_amount,
        script_pubkey: contract_script,
    };
    let prevouts = vec![prevout];
    let prevouts_ref = Prevouts::All(&prevouts);

    // Calculate sighash
    let mut spending_tx_copy = spending_tx.clone();
    let mut sighasher = SighashCache::new(&mut spending_tx_copy);
    let sighash = sighasher
        .taproot_key_spend_signature_hash(0, &prevouts_ref, bitcoin::TapSighashType::Default)
        .map_err(|_| ProtocolError::General("Failed to compute sighash"))?;

    Ok(bitcoin::secp256k1::Message::from(sighash))
}

pub(crate) fn calculate_coinswap_fee(
    swap_amount: u64,
    refund_locktime: u16,
    base_fee: u64,
    amt_rel_fee_pct: f64,
    time_rel_fee_pct: f64,
) -> u64 {
    // swap_amount as f64 * refund_locktime as f64 -> can  overflow inside f64?
    let total_fee = base_fee as f64
        + (swap_amount as f64 * amt_rel_fee_pct) / 1_00.00
        + (swap_amount as f64 * refund_locktime as f64 * time_rel_fee_pct) / 1_00.00;

    total_fee.ceil() as u64
}

#[cfg(test)]
mod tests {

    use bitcoin::{
        blockdata::transaction::{OutPoint, TxIn},
        secp256k1::{Keypair, Message, Scalar},
        taproot::{LeafVersion, TapLeafHash},
        transaction::Version,
        Sequence, Witness,
    };
    use secp256k1::musig::{AggregatedNonce, PublicNonce};

    fn create_unsigned_contract_tx(
        input: OutPoint,
        input_value: Amount,
        script_pubkey: ScriptBuf,
        fee: Amount,
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
            version: Version::TWO,
        })
    }

    use crate::protocol::musig_interface::{
        aggregate_partial_signatures_i, generate_new_nonce_pair_i, generate_partial_signature_i,
        get_aggregated_nonce_i, get_aggregated_pubkey_i,
    };
    use bitcoin::{
        sighash::Prevouts,
        taproot::{ControlBlock, TaprootSpendInfo},
        Address, TapSighashType,
    };
    use bitcoind::bitcoincore_rpc::{
        json::ListUnspentResultEntry, Auth::UserPass, Client, RawTx, RpcApi,
    };
    use secp256k1::musig::{AggregatedSignature, SecretNonce};
    use std::str::FromStr;

    use super::*;
    use bitcoin::Txid;

    fn mine_blocks(num: u64, address: &Address) {
        let client = Client::new(
            "127.0.0.1:21443/wallet/test_wallet",
            UserPass("user".to_string(), "pass".to_string()),
        )
        .unwrap();
        client.generate_to_address(num, address).unwrap();
    }

    // #[test]
    fn create_and_fund_address() -> (Address, ListUnspentResultEntry) {
        // Create a new address using bitcoin core rpc
        let client = Client::new(
            "127.0.0.1:21443/wallet/test_wallet",
            UserPass("user".to_string(), "pass".to_string()),
        )
        .unwrap();
        // let blockchian_info = client.get_blockchain_info().unwrap();
        // println!("Blockchain info: {:?}", blockchian_info);
        let wallets = client.list_wallets().unwrap();
        // println!("Wallets: {:?}", wallets);
        let wallet_name = "test_wallet";
        if !wallets.contains(&wallet_name.to_string()) {
            client
                .create_wallet(wallet_name, None, None, None, None)
                .unwrap();
        }
        // client.load_wallet(wallet_name).unwrap();
        let address = client.get_new_address(None, None).unwrap();
        let checked_address = address.require_network(bitcoin::Network::Regtest).unwrap();
        mine_blocks(101, &checked_address);
        let balance = client.get_balances().unwrap();
        println!("Balance: {:?}", balance);
        let utxos = client
            .list_unspent(None, None, Some(&[&checked_address]), None, None)
            .unwrap();
        // println!("UTXOs: {:?}", utxos);
        (checked_address, utxos.get(0).unwrap().clone())
    }

    use bitcoin::hashes::{sha256 as Sha256, Hash};
    // #[test]
    fn create_and_broadcast_contract(
        spending_utxo: ListUnspentResultEntry,
        internal_key: XOnlyPublicKey,
        hashlock_pubkey: XOnlyPublicKey,
        timelock_pubkey: XOnlyPublicKey,
        hash_preimage: [u8; 32],
        locktime: LockTime,
    ) -> (Txid, TaprootSpendInfo, ScriptBuf, ScriptBuf) {
        println!("Hash Preimage: {:?}", hash_preimage);
        let hash = Sha256::Hash::hash(&hash_preimage);
        println!("Hash: {:?}", hash);
        let hashlock_script = create_hashlock_script(&hash.as_byte_array(), &hashlock_pubkey);
        println!("Hashlock script: {:?}", hashlock_script);
        let timelock_script = create_timelock_script(locktime, &timelock_pubkey);
        println!("Timelock script: {:?}", timelock_script);
        let (taproot_script, taproot_spendinfo) = create_taproot_script(
            hashlock_script.clone(),
            timelock_script.clone(),
            internal_key,
        );
        println!("Taproot script ASM {:?}", taproot_script.to_asm_string());
        let spending_txid = spending_utxo.txid;
        let spending_vout = spending_utxo.vout;
        let spending_value = spending_utxo.amount;
        let transaction = create_unsigned_contract_tx(
            OutPoint {
                txid: spending_txid,
                vout: spending_vout,
            },
            spending_value,
            taproot_script.clone(),
            Amount::from_sat(1000),
        )
        .unwrap();
        (
            sign_seed_transaction(&transaction),
            taproot_spendinfo,
            hashlock_script,
            timelock_script,
        )
    }

    fn sign_seed_transaction(tx: &Transaction) -> Txid {
        let client = Client::new(
            "127.0.0.1:21443/wallet/test_wallet",
            UserPass("user".to_string(), "pass".to_string()),
        )
        .unwrap();
        let signed_tx = client
            .sign_raw_transaction_with_wallet(tx, None, None)
            .unwrap();
        println!("Signed transaction: {:?}", signed_tx);
        client.send_raw_transaction(&signed_tx.hex).unwrap()
    }

    fn sweep_via_internal_key(
        outpoint: OutPoint,
        keypair1: Keypair,
        keypair2: Keypair,
        tap_tweak: Scalar,
        output_address: Address,
    ) {
        let client = Client::new(
            "127.0.0.1:21443/wallet/test_wallet",
            UserPass("user".to_string(), "pass".to_string()),
        )
        .unwrap();
        let input_txid = &outpoint.txid;
        let txin = TxIn {
            previous_output: outpoint,
            script_sig: ScriptBuf::new(),
            sequence: Sequence::ZERO,
            witness: Witness::default(),
        };
        let prevout = client.get_tx_out(input_txid, 0, None).unwrap().unwrap();
        let prevout_value = prevout.value;
        let prev_txout = TxOut {
            script_pubkey: ScriptBuf::from_bytes(prevout.script_pub_key.hex),
            value: prevout_value,
        };
        let output = TxOut {
            script_pubkey: output_address.script_pubkey(),
            value: prevout_value - Amount::from_sat(1000),
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
        let sighash = sighasher
            .taproot_key_spend_signature_hash(0, &prevouts, sighash_type)
            .expect("Failed to compute sighash");
        let msg = Message::from(sighash);
        println!("Sighash: {:?}", msg);

        let pubkey1 = keypair1.public_key();
        let pubkey2 = keypair2.public_key();
        // let signature = secp.sign_schnorr(&msg, &tweaked.to_inner());
        let nonce_pair_1: (SecretNonce, PublicNonce) = generate_new_nonce_pair_i(pubkey1);
        let nonce_pair_2: (SecretNonce, PublicNonce) = generate_new_nonce_pair_i(pubkey2);
        let agg_nonce: AggregatedNonce =
            get_aggregated_nonce_i(&vec![&nonce_pair_1.1, &nonce_pair_2.1]);

        let partial_signature_1 = generate_partial_signature_i(
            msg,
            &agg_nonce,
            nonce_pair_1.0,
            keypair1,
            tap_tweak,
            pubkey1,
            pubkey2,
        );
        let partial_signature_2 = generate_partial_signature_i(
            msg,
            &agg_nonce,
            nonce_pair_2.0,
            keypair2,
            tap_tweak,
            pubkey1,
            pubkey2,
        );

        let aggregated_signature: AggregatedSignature = aggregate_partial_signatures_i(
            msg,
            agg_nonce,
            tap_tweak,
            vec![&partial_signature_1, &partial_signature_2],
            pubkey1,
            pubkey2,
        );
        // let signature = bitcoin::taproot::Signature { signature, sighash_type};
        // let signature = [146, 123, 43, 11, 8, 25, 106, 52, 146, 170, 93, 164, 241, 65, 172, 150, 242, 122, 30, 219, 154, 177, 9, 40, 105, 228, 213, 77, 219, 43, 139, 35, 176, 185, 230, 70, 213, 228, 160, 12, 144, 149, 191, 113, 210, 77, 137, 251, 2, 250, 109, 0, 69, 244, 156, 135, 122, 231, 227, 208, 78, 237, 142, 191];
        let signature = bitcoin::taproot::Signature::from_slice(
            aggregated_signature.assume_valid().as_byte_array(),
        )
        .unwrap();
        *sighasher.witness_mut(0).unwrap() = Witness::p2tr_key_spend(&signature);

        let tx = sighasher.into_transaction();
        println!("{:#?}", tx);
        let txid = client.send_raw_transaction(tx.raw_hex());
        println!("Transaction ID: {:?}", txid);
    }

    fn sweep_via_hashlock(
        outpoint: OutPoint,
        hash_preimage: [u8; 32],
        hashlock_keypair: Keypair,
        control_block: ControlBlock,
    ) {
        let secp = Secp256k1::new();
        let client = Client::new(
            "127.0.0.1:21443/wallet/test_wallet",
            UserPass("user".to_string(), "pass".to_string()),
        )
        .unwrap();
        let output_address = Address::from_str("bcrt1qdcm6dj28v9jm9f2ykqkkntwe5khjtvdf8ldu6d")
            .unwrap()
            .require_network(bitcoin::Network::Regtest)
            .unwrap();
        let prevout_txid = outpoint.txid;
        let txin = TxIn {
            previous_output: outpoint,
            script_sig: ScriptBuf::new(),
            sequence: Sequence::ZERO,
            witness: Witness::default(),
        };
        let prevout = client.get_tx_out(&prevout_txid, 0, None).unwrap().unwrap();
        let prevout_value = prevout.value;
        let prev_txout = TxOut {
            script_pubkey: ScriptBuf::from_bytes(prevout.script_pub_key.hex),
            value: prevout_value,
        };
        let output = TxOut {
            script_pubkey: output_address.script_pubkey(),
            value: prevout_value - Amount::from_sat(1000),
        };
        let mut unsigned_transaction = Transaction {
            input: vec![txin],
            output: vec![output.clone()],
            lock_time: LockTime::ZERO,
            version: Version::TWO,
        };

        let (x_only_pubkey, _) = hashlock_keypair.x_only_public_key();
        let hash = Sha256::Hash::hash(&hash_preimage);
        let hashlock_script = create_hashlock_script(&hash.as_byte_array(), &x_only_pubkey);

        let sighash_type = TapSighashType::All;
        let prevouts = vec![prev_txout];
        let prevouts = Prevouts::All(&prevouts);
        let mut sighasher = SighashCache::new(&mut unsigned_transaction);
        let sighash = sighasher
            .taproot_script_spend_signature_hash(
                0,
                &prevouts,
                TapLeafHash::from_script(&hashlock_script, LeafVersion::TapScript),
                sighash_type,
            )
            .expect("Failed to compute sighash");
        let msg = Message::from(sighash);
        let signature = secp.sign_schnorr(&msg, &hashlock_keypair);

        let signature = bitcoin::taproot::Signature {
            signature,
            sighash_type,
        };
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

    fn sweep_via_timelock(
        outpoint: OutPoint,
        timelock: LockTime,
        timelock_keypair: Keypair,
        control_block: ControlBlock,
    ) {
        let secp = Secp256k1::new();
        let client = Client::new(
            "127.0.0.1:21443/wallet/test_wallet",
            UserPass("user".to_string(), "pass".to_string()),
        )
        .unwrap();
        let output_address = Address::from_str("bcrt1qdcm6dj28v9jm9f2ykqkkntwe5khjtvdf8ldu6d")
            .unwrap()
            .require_network(bitcoin::Network::Regtest)
            .unwrap();
        let txin = TxIn {
            previous_output: outpoint,
            script_sig: ScriptBuf::new(),
            sequence: Sequence::ZERO,
            witness: Witness::default(),
        };
        let prevout_txid = outpoint.txid;
        let prevout = client.get_tx_out(&prevout_txid, 0, None).unwrap().unwrap();
        let prevout_value = prevout.value;
        let prev_txout = TxOut {
            script_pubkey: ScriptBuf::from_bytes(prevout.script_pub_key.hex),
            value: prevout_value,
        };
        let output = TxOut {
            script_pubkey: output_address.script_pubkey(),
            value: prevout_value - Amount::from_sat(1000),
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
        let sighash = sighasher
            .taproot_script_spend_signature_hash(0, &prevouts_all, script_leaf_hash, sighash_type)
            .expect("Failed to compute sighash for timelock script spend");

        let msg = Message::from(sighash);
        let signature = secp.sign_schnorr(&msg, &timelock_keypair);

        let taproot_signature = bitcoin::taproot::Signature {
            signature,
            sighash_type,
        };
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

    // #[test]
    fn end_to_end_internal_key() {
        let secp = Secp256k1::new();

        let seckey1_bytes = [
            53, 126, 153, 168, 20, 2, 57, 61, 57, 192, 65, 188, 170, 70, 195, 245, 0, 137, 135, 59,
            128, 104, 181, 90, 187, 118, 160, 138, 217, 172, 220, 56,
        ];
        let seckey2_bytes = [
            87, 32, 109, 105, 102, 136, 254, 135, 248, 148, 13, 5, 127, 89, 5, 64, 49, 245, 51,
            224, 211, 94, 101, 150, 225, 7, 68, 134, 79, 188, 167, 235,
        ];
        let keypair_1 = Keypair::from_seckey_slice(&secp, &seckey1_bytes).unwrap();
        let keypair_2 = Keypair::from_seckey_slice(&secp, &seckey2_bytes).unwrap();
        let pubkey1 = keypair_1.public_key();
        let pubkey2 = keypair_2.public_key();
        let internal_key = get_aggregated_pubkey_i(pubkey1, pubkey2);

        let timelock_keypair_slice = [
            57, 36, 177, 212, 31, 75, 221, 50, 13, 55, 102, 155, 21, 64, 146, 106, 101, 189, 1,
            167, 80, 10, 246, 136, 96, 80, 57, 67, 63, 194, 67, 241,
        ];
        let timelock_keypair = Keypair::from_seckey_slice(&secp, &timelock_keypair_slice).unwrap();
        let (timelock_pubkey, _) = timelock_keypair.x_only_public_key();

        let hashlock_keypair_slice = [
            196, 70, 74, 243, 36, 44, 31, 223, 8, 77, 47, 179, 229, 217, 187, 66, 144, 87, 50, 11,
            184, 136, 255, 92, 97, 154, 183, 102, 27, 126, 54, 140,
        ];
        let hashlock_keypair = Keypair::from_seckey_slice(&secp, &hashlock_keypair_slice).unwrap();
        let (hashlock_pubkey, _) = hashlock_keypair.x_only_public_key();

        let hash_preimage = [0u8; 32];
        let locktime = LockTime::from_height(1000).unwrap();

        let (address, spending_utxo) = create_and_fund_address();
        let (contract_txid, taproot_spendinfo, _hashlock_script, _timelock_script) =
            create_and_broadcast_contract(
                spending_utxo,
                internal_key,
                hashlock_pubkey,
                timelock_pubkey,
                hash_preimage,
                locktime,
            );
        mine_blocks(1, &address);
        println!("Contract created and mined successfully");

        let contract_outpoint = OutPoint {
            txid: contract_txid,
            vout: 0,
        };

        sweep_via_internal_key(
            contract_outpoint,
            keypair_1,
            keypair_2,
            taproot_spendinfo.tap_tweak().to_scalar(),
            address,
        );
        println!("Contract successfully spent via musig internal key");
    }

    // #[test]
    fn end_to_end_hashlock() {
        let secp = Secp256k1::new();

        let seckey1_bytes = [
            53, 126, 153, 168, 20, 2, 57, 61, 57, 192, 65, 188, 170, 70, 195, 245, 0, 137, 135, 59,
            128, 104, 181, 90, 187, 118, 160, 138, 217, 172, 220, 56,
        ];
        let seckey2_bytes = [
            87, 32, 109, 105, 102, 136, 254, 135, 248, 148, 13, 5, 127, 89, 5, 64, 49, 245, 51,
            224, 211, 94, 101, 150, 225, 7, 68, 134, 79, 188, 167, 235,
        ];
        let keypair_1 = Keypair::from_seckey_slice(&secp, &seckey1_bytes).unwrap();
        let keypair_2 = Keypair::from_seckey_slice(&secp, &seckey2_bytes).unwrap();
        let pubkey1 = keypair_1.public_key();
        let pubkey2 = keypair_2.public_key();
        let internal_key = get_aggregated_pubkey_i(pubkey1, pubkey2);

        let timelock_keypair_slice = [
            57, 36, 177, 212, 31, 75, 221, 50, 13, 55, 102, 155, 21, 64, 146, 106, 101, 189, 1,
            167, 80, 10, 246, 136, 96, 80, 57, 67, 63, 194, 67, 241,
        ];
        let timelock_keypair = Keypair::from_seckey_slice(&secp, &timelock_keypair_slice).unwrap();
        let (timelock_pubkey, _) = timelock_keypair.x_only_public_key();

        let hashlock_keypair_slice = [
            196, 70, 74, 243, 36, 44, 31, 223, 8, 77, 47, 179, 229, 217, 187, 66, 144, 87, 50, 11,
            184, 136, 255, 92, 97, 154, 183, 102, 27, 126, 54, 140,
        ];
        let hashlock_keypair = Keypair::from_seckey_slice(&secp, &hashlock_keypair_slice).unwrap();
        let (hashlock_pubkey, _) = hashlock_keypair.x_only_public_key();

        let hash_preimage = [0u8; 32];
        let locktime = LockTime::from_height(1000).unwrap();

        let (address, spending_utxo) = create_and_fund_address();
        let (contract_txid, taproot_spendinfo, hashlock_script, _timelock_script) =
            create_and_broadcast_contract(
                spending_utxo,
                internal_key,
                hashlock_pubkey,
                timelock_pubkey,
                hash_preimage,
                locktime,
            );
        mine_blocks(1, &address);
        println!("Contract created and mined successfully");

        let contract_outpoint = OutPoint {
            txid: contract_txid,
            vout: 0,
        };

        sweep_via_hashlock(
            contract_outpoint,
            hash_preimage,
            hashlock_keypair,
            taproot_spendinfo
                .control_block(&(hashlock_script, LeafVersion::TapScript))
                .unwrap(),
        );
        println!("Contract successfully spent via hashlock");
    }

    // #[test]
    fn end_to_end_timelock() {
        let secp = Secp256k1::new();

        let seckey1_bytes = [
            53, 126, 153, 168, 20, 2, 57, 61, 57, 192, 65, 188, 170, 70, 195, 245, 0, 137, 135, 59,
            128, 104, 181, 90, 187, 118, 160, 138, 217, 172, 220, 56,
        ];
        let seckey2_bytes = [
            87, 32, 109, 105, 102, 136, 254, 135, 248, 148, 13, 5, 127, 89, 5, 64, 49, 245, 51,
            224, 211, 94, 101, 150, 225, 7, 68, 134, 79, 188, 167, 235,
        ];
        let keypair_1 = Keypair::from_seckey_slice(&secp, &seckey1_bytes).unwrap();
        let keypair_2 = Keypair::from_seckey_slice(&secp, &seckey2_bytes).unwrap();
        let pubkey1 = keypair_1.public_key();
        let pubkey2 = keypair_2.public_key();
        let internal_key = get_aggregated_pubkey_i(pubkey1, pubkey2);

        let timelock_keypair_slice = [
            57, 36, 177, 212, 31, 75, 221, 50, 13, 55, 102, 155, 21, 64, 146, 106, 101, 189, 1,
            167, 80, 10, 246, 136, 96, 80, 57, 67, 63, 194, 67, 241,
        ];
        let timelock_keypair = Keypair::from_seckey_slice(&secp, &timelock_keypair_slice).unwrap();
        let (timelock_pubkey, _) = timelock_keypair.x_only_public_key();

        let hashlock_keypair_slice = [
            196, 70, 74, 243, 36, 44, 31, 223, 8, 77, 47, 179, 229, 217, 187, 66, 144, 87, 50, 11,
            184, 136, 255, 92, 97, 154, 183, 102, 27, 126, 54, 140,
        ];
        let hashlock_keypair = Keypair::from_seckey_slice(&secp, &hashlock_keypair_slice).unwrap();
        let (hashlock_pubkey, _) = hashlock_keypair.x_only_public_key();

        let hash_preimage = [0u8; 32];
        let locktime = LockTime::from_height(1000).unwrap();

        let (address, spending_utxo) = create_and_fund_address();
        let (contract_txid, taproot_spendinfo, _hashlock_script, timelock_script) =
            create_and_broadcast_contract(
                spending_utxo,
                internal_key,
                hashlock_pubkey,
                timelock_pubkey,
                hash_preimage,
                locktime,
            );
        mine_blocks(1, &address);
        println!("Contract created and mined successfully");

        let contract_outpoint = OutPoint {
            txid: contract_txid,
            vout: 0,
        };

        mine_blocks(1000, &address);
        sweep_via_timelock(
            contract_outpoint,
            locktime,
            timelock_keypair,
            taproot_spendinfo
                .control_block(&(timelock_script, LeafVersion::TapScript))
                .unwrap(),
        );
        println!("Contract successfully spent via hashlock");
    }

    #[test]
    #[ignore]
    fn end_to_end() {
        end_to_end_internal_key();
        end_to_end_hashlock();
        end_to_end_timelock();
    }
}

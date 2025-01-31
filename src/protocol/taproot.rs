use bitcoin::{
    Address, Amount, LockTime, Network, OutPoint, Sequence, 
    Script as ScriptBuf, Transaction, TxIn, TxOut, Version, Witness,
    opcodes::{OP_CHECKSIG, OP_CHECKSIGVERIFY, OP_CLTV, OP_EQUALVERIFY, OP_HASH160},
    secp256k1::{Secp256k1, XOnlyPublicKey},
    taproot::{TaprootBuilder, TaprootSpendInfo, LeafVersion},
    script::Builder,
    hash160,
};

use crate::error::Error;
use crate::types::{ElectrumConfig, Fee, Preimage};

/// Represents a CoinSwap Taproot contract
pub struct CoinSwapTaproot {
    pub spend_info: TaprootSpendInfo,
    pub hashlock_script: ScriptBuf,
    pub timelock_script: ScriptBuf,
}

impl CoinSwapTaproot {
    /// Create new Taproot contract with hashlock and timelock scripts
    pub fn new(
        sender_pubkey: XOnlyPublicKey,
        receiver_pubkey: XOnlyPublicKey,
        hashlock: hash160::Hash,
        locktime: LockTime,
    ) -> Result<Self, Error> {
        let secp = Secp256k1::new();
        
        // Build script leaves
        let hashlock_script = Self::build_hashlock_script(receiver_pubkey, hashlock);
        let timelock_script = Self::build_timelock_script(sender_pubkey, locktime);

        // Construct Taproot tree
        let internal_key = sender_pubkey; // Simplified, would normally be aggregated key
        let taproot_builder = TaprootBuilder::new()
            .add_leaf(1, hashlock_script.clone())?
            .add_leaf(1, timelock_script.clone())?;

        let spend_info = taproot_builder.finalize(&secp, internal_key)?;

        Ok(Self {
            spend_info,
            hashlock_script,
            timelock_script
        })
    }

    /// Build hashlock script: OP_HASH160 <hash> OP_EQUALVERIFY <pubkey> OP_CHECKSIG
    fn build_hashlock_script(pubkey: XOnlyPublicKey, hash: hash160::Hash) -> ScriptBuf {
        Builder::new()
            .push_opcode(OP_HASH160)
            .push_slice(hash)
            .push_opcode(OP_EQUALVERIFY)
            .push_x_only_key(&pubkey)
            .push_opcode(OP_CHECKSIG)
            .into_script()
    }

    /// Build timelock script: <pubkey> OP_CHECKSIGVERIFY <locktime> OP_CLTV
    fn build_timelock_script(pubkey: XOnlyPublicKey, locktime: LockTime) -> ScriptBuf {
        Builder::new()
            .push_x_only_key(&pubkey)
            .push_opcode(OP_CHECKSIGVERIFY)
            .push_lock_time(locktime)
            .push_opcode(OP_CLTV)
            .into_script()
    }

    /// Get Taproot address for funding
    pub fn address(&self, network: Network) -> Address {
        Address::p2tr_tweaked(self.spend_info.output_key(), network)
    }
}

/// Create funding transaction with Taproot output
pub fn create_funding_tx(
    inputs: Vec<TxIn>,
    taproot_address: &Address,
    amount: Amount,
    change_script: ScriptBuf,
    fee_rate: f32,
) -> Result<Transaction, Error> {
    // Implementation similar to your existing create_tx_with_fee
    // but specifically for creating the initial funding output
}

/// Create claim transaction spending through hashlock path
pub fn create_claim_tx(
    funding_outpoint: OutPoint,
    funding_amount: Amount,
    recipient_address: Address,
    preimage: &Preimage,
    fee_rate: f32,
) -> Result<Transaction, Error> {
    // Base transaction structure
    let mut tx = Transaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: funding_outpoint,
            ..Default::default()
        }],
        output: vec![],
    };

    // Add output and calculate fee
    create_tx_with_fee(
        Fee::Rate(fee_rate),
        |fee| {
            let amount_after_fee = funding_amount.checked_sub(fee).ok_or(Error::InsufficientFunds)?;
            tx.output = vec![TxOut {
                value: amount_after_fee,
                script_pubkey: recipient_address.script_pubkey(),
            }];
            Ok(tx.clone())
        },
        |tx| tx.vsize(),
    )
}

/// Create refund transaction spending through timelock path  
pub fn create_refund_tx(
    funding_outpoint: OutPoint,
    funding_amount: Amount,
    refund_address: Address,
    locktime: LockTime,
    fee_rate: f32,
) -> Result<Transaction, Error> {
    let mut tx = Transaction {
        version: Version::TWO,
        lock_time: locktime,
        input: vec![TxIn {
            previous_output: funding_outpoint,
            sequence: Sequence::ZERO,
            ..Default::default()
        }],
        output: vec![],
    };

    create_tx_with_fee(
        Fee::Rate(fee_rate),
        |fee| {
            let amount_after_fee = funding_amount.checked_sub(fee).ok_or(Error::InsufficientFunds)?;
            tx.output = vec![TxOut {
                value: amount_after_fee,
                script_pubkey: refund_address.script_pubkey(),
            }];
            Ok(tx.clone())
        },
        |tx| tx.vsize(),
    )
}

/// Create witness for hashlock script path spending
pub fn create_hashlock_witness(
    signature: Signature,
    preimage: &Preimage,
    taproot: &CoinSwapTaproot,
) -> Result<Witness, Error> {
    let control_block = taproot.spend_info
        .control_block(&(taproot.hashlock_script.clone(), LeafVersion::TapScript))
        .ok_or(Error::Taproot("Control block not found".into()))?;

    let mut witness = Witness::new();
    witness.push(signature.to_vec());
    witness.push(preimage.as_bytes());
    witness.push(taproot.hashlock_script.as_bytes());
    witness.push(control_block.serialize());

    Ok(witness)
}

/// Create witness for timelock script path spending
pub fn create_timelock_witness(
    signature: Signature,
    taproot: &CoinSwapTaproot,
) -> Result<Witness, Error> {
    let control_block = taproot.spend_info
        .control_block(&(taproot.timelock_script.clone(), LeafVersion::TapScript))
        .ok_or(Error::Taproot("Control block not found".into()))?;

    let mut witness = Witness::new();
    witness.push(signature.to_vec());
    witness.push(taproot.timelock_script.as_bytes());
    witness.push(control_block.serialize());

    Ok(witness)
}

/// Validate hashlock script requirements
pub fn validate_hashlock(
    preimage: &Preimage,
    expected_hash: &hash160::Hash,
) -> Result<(), Error> {
    let hash = hash160::Hash::hash(preimage.as_bytes());
    if &hash != expected_hash {
        return Err(Error::Protocol("Invalid preimage for hashlock".into()));
    }
    Ok(())
}

/// Validate timelock requirements
pub fn validate_timelock(
    tx_locktime: LockTime,
    current_height: u32,
) -> Result<(), Error> {
    if tx_locktime.to_consensus_u32() > current_height {
        return Err(Error::Protocol("Timelock not yet expired".into()));
    }
    Ok(())
}

/// Broadcast completed transaction
pub fn broadcast_transaction(
    tx: &Transaction,
    electrum_config: &ElectrumConfig,
) -> Result<Txid, Error> {
    let client = electrum_config.build_client()?;
    client.transaction_broadcast(tx).map_err(|e| e.into())
}


// Flow of the CoinSwap protocol
// let alice_pubkey = XOnlyPublicKey::from_str("...")?;
// let bob_pubkey = XOnlyPublicKey::from_str("...")?;
// let preimage = Preimage::new([...]);
// let hash = hash160::Hash::hash(preimage.as_bytes());

// // Create Taproot contract
// let taproot = CoinSwapTaproot::new(
//     alice_pubkey,
//     bob_pubkey,
//     hash,
//     LockTime::from_height(1000)?,
// )?;

// // Get funding address
// let funding_address = taproot.address(Network::Bitcoin);


// let funding_tx = create_funding_tx(
//     inputs,
//     &funding_address,
//     Amount::from_btc(1.0)?,
//     change_script,
//     2.5, // sat/vbyte
// )?;

// let claim_tx = create_claim_tx(
//     funding_outpoint,
//     funding_amount,
//     recipient_address,
//     &preimage,
//     2.5,
// )?;

// let signature = sign_claim_tx(&key, &claim_tx)?;
// let witness = create_hashlock_witness(signature, &preimage, &taproot)?;
// claim_tx.input[0].witness = witness;

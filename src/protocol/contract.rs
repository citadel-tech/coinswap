use std::convert::TryInto;

use bitcoin::{
    absolute::LockTime,
    blockdata::{
        opcodes::{self, all},
        script::{Builder, Instruction, Script},
    },
    hashes::Hash,
    secp256k1::{
        ecdsa::Signature,
        rand::{rngs::OsRng, RngCore},
        Message, Secp256k1, SecretKey,
    },
    sighash::{EcdsaSighashType, SighashCache},
    OutPoint, PublicKey, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Witness,
};

pub use bitcoin::hashes::hash160::Hash as Hash160;

use crate::utill::redeemscript_to_scriptpubkey;

use super::{
    error::ContractError,
    messages::{FundingTxInfo, ProofOfFunding},
};

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
pub const FUNDING_TX_VBYTE_SIZE: u64 = 372;

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
    let (sig_first, sig_second) = if key1.inner.serialize()[..] < key2.inner.serialize()[..] {
        (sig1, sig2)
    } else {
        (sig2, sig1)
    };

    let mut sig1_with_sighash = sig_first.serialize_der().to_vec();
    sig1_with_sighash.push(EcdsaSighashType::All as u8);

    let mut sig2_with_sighash = sig_second.serialize_der().to_vec();
    sig2_with_sighash.push(EcdsaSighashType::All as u8);

    input.witness.push(Vec::new()); //first is multisig dummy
    input.witness.push(sig1_with_sighash);
    input.witness.push(sig2_with_sighash);
    input.witness.push(redeemscript.to_bytes());
}

pub fn create_multisig_redeemscript(key1: &PublicKey, key2: &PublicKey) -> ScriptBuf {
    let builder = Builder::new().push_opcode(all::OP_PUSHNUM_2);
    if key1.inner.serialize()[..] < key2.inner.serialize()[..] {
        builder.push_key(key1).push_key(key2)
    } else {
        builder.push_key(key2).push_key(key1)
    }
    .push_opcode(all::OP_PUSHNUM_2)
    .push_opcode(all::OP_CHECKMULTISIG)
    .into_script()
}

pub fn derive_maker_pubkey_and_nonce(
    tweakable_point: &PublicKey,
) -> Result<(PublicKey, SecretKey), ContractError> {
    let mut nonce_bytes = [0u8; 32];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = SecretKey::from_slice(&nonce_bytes)?;
    let maker_pubkey = calculate_pubkey_from_nonce(tweakable_point, &nonce)?;
    Ok((maker_pubkey, nonce))
}

pub fn calculate_pubkey_from_nonce(
    tweakable_point: &PublicKey,
    nonce: &SecretKey,
) -> Result<PublicKey, ContractError> {
    let secp = Secp256k1::new();

    let nonce_point = bitcoin::secp256k1::PublicKey::from_secret_key(&secp, nonce);
    Ok(PublicKey {
        compressed: true,
        inner: tweakable_point.inner.combine(&nonce_point)?,
    })
}

// TODO: Just return the index, TxOut can be found from there.
pub fn find_funding_output_index(funding_tx_info: &FundingTxInfo) -> Result<u32, ContractError> {
    let multisig_spk = redeemscript_to_scriptpubkey(&funding_tx_info.multisig_redeemscript);
    funding_tx_info
        .funding_tx
        .output
        .iter()
        .enumerate()
        .find(|(_i, o)| o.script_pubkey == multisig_spk)
        .map(|(index, _)| index as u32)
        .ok_or(ContractError::Protocol(
            "Funding output doesn't match with multisig reedimscript",
        ))
}

pub fn check_reedemscript_is_multisig(redeemscript: &Script) -> Result<(), ContractError> {
    //pattern match to check redeemscript is really a 2of2 multisig
    let mut ms_rs_bytes = redeemscript.to_bytes();
    const PUB_PLACEHOLDER: [u8; 33] = [0x02; 33];
    let pubkey_placeholder = PublicKey::from_slice(&PUB_PLACEHOLDER).unwrap();
    let template_ms_rs =
        create_multisig_redeemscript(&pubkey_placeholder, &pubkey_placeholder).into_bytes();
    if ms_rs_bytes.len() != template_ms_rs.len() {
        return Err(ContractError::Protocol(
            "wrong multisig_redeemscript length",
        ));
    }
    ms_rs_bytes.splice(2..35, PUB_PLACEHOLDER.iter().cloned());
    ms_rs_bytes.splice(36..69, PUB_PLACEHOLDER.iter().cloned());
    if ms_rs_bytes != template_ms_rs {
        return Err(ContractError::Protocol(
            "redeemscript not matching multisig template",
        ));
    } else {
        Ok(())
    }
}

pub fn check_multisig_has_pubkey(
    redeemscript: &Script,
    tweakable_point: &PublicKey,
    nonce: &SecretKey,
) -> Result<(), ContractError> {
    let (pubkey1, pubkey2) = read_pubkeys_from_multisig_redeemscript(redeemscript)?;
    let my_pubkey = calculate_pubkey_from_nonce(tweakable_point, nonce)?;
    if pubkey1 != my_pubkey && pubkey2 != my_pubkey {
        return Err(ContractError::Protocol(
            "wrong pubkeys in multisig_redeemscript",
        ));
    } else {
        Ok(())
    }
}

pub fn check_hashlock_has_pubkey(
    contract_redeemscript: &Script,
    tweakable_point: &PublicKey,
    nonce: &SecretKey,
) -> Result<(), ContractError> {
    let contract_hashlock_pubkey = read_hashlock_pubkey_from_contract(contract_redeemscript)?;
    let derived_hashlock_pubkey = calculate_pubkey_from_nonce(tweakable_point, nonce)?;
    if contract_hashlock_pubkey != derived_hashlock_pubkey {
        return Err(ContractError::Protocol(
            "contract hashlock pubkey doesnt match with key derived from nonce",
        ));
    } else {
        Ok(())
    }
}

#[rustfmt::skip]
pub fn create_contract_redeemscript(
    pub_hashlock: &PublicKey,
    pub_timelock: &PublicKey,
    hashvalue: &Hash160,
    locktime: &u16,
) -> ScriptBuf {
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
        .push_slice(hashvalue.to_byte_array())
        .push_opcode(opcodes::all::OP_EQUAL)
        .push_opcode(opcodes::all::OP_IF)
            .push_key(&pub_hashlock)
            .push_int(32)
            .push_int(1)
        .push_opcode(opcodes::all::OP_ELSE)
            .push_key(&pub_timelock)
            .push_int(0)
            .push_int(*locktime as i64)
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
pub fn read_hashvalue_from_contract(redeemscript: &Script) -> Result<Hash160, ContractError> {
    if redeemscript.to_bytes().len() < 25 {
        return Err(ContractError::Protocol("contract reedemscript too short"));
    }
    Ok(Hash160::from_slice(
        redeemscript.to_bytes()[4..24]
            .try_into()
            .map_err(|_| ContractError::Protocol("hash value is not 20 bytes slice"))?,
    )?)
}

//check that all the contract redeemscripts involve the same hashvalue
pub fn check_hashvalues_are_equal(message: &ProofOfFunding) -> Result<Hash160, ContractError> {
    let hashvalues = message
        .confirmed_funding_txes
        .iter()
        .map(|funding_info| {
            Ok(read_hashvalue_from_contract(
                &funding_info.contract_redeemscript,
            )?)
        })
        .collect::<Result<Vec<_>, ContractError>>()?;

    if !hashvalues.iter().all(|value| value == &hashvalues[0]) {
        return Err(ContractError::Protocol(
            "contract reedemscript doesn't have equal hashvalues",
        ));
    }

    Ok(hashvalues[0])
}

pub fn read_contract_locktime(redeemscript: &Script) -> Result<u16, ContractError> {
    match redeemscript
        .instructions()
        .nth(12)
        .expect("Insctructions expected")?
    {
        Instruction::PushBytes(locktime_bytes) => match locktime_bytes.len() {
            1 => Ok(locktime_bytes[0] as u16),
            2 | 3 => {
                let (int_bytes, _rest) = locktime_bytes
                    .as_bytes()
                    .split_at(std::mem::size_of::<u16>());
                Ok(u16::from_le_bytes(int_bytes.try_into().unwrap()))
            }
            _ => Err(ContractError::Protocol(
                "Can't read locktime value from contract reedemscript",
            )),
        },
        Instruction::Op(opcode) => {
            if let opcodes::Class::PushNum(n) = opcode.classify(opcodes::ClassifyContext::Legacy) {
                Ok(n.try_into().map_err(|_| {
                    ContractError::Protocol("Can't read locktime value from contract reedemscript")
                })?)
            } else {
                Err(ContractError::Protocol(
                    "Can't read locktime value from contract reedemscript",
                ))
            }
        }
    }
}

pub fn read_hashlock_pubkey_from_contract(
    redeemscript: &Script,
) -> Result<PublicKey, ContractError> {
    if redeemscript.to_bytes().len() < 61 {
        return Err(ContractError::Protocol("contract reedemscript too short"));
    }
    Ok(PublicKey::from_slice(&redeemscript.to_bytes()[27..60])?)
}

pub fn read_timelock_pubkey_from_contract(
    redeemscript: &Script,
) -> Result<PublicKey, ContractError> {
    if redeemscript.to_bytes().len() < 99 {
        return Err(ContractError::Protocol("contract reedemscript too short"));
    }
    Ok(PublicKey::from_slice(&redeemscript.to_bytes()[65..98])?)
}

pub fn read_pubkeys_from_multisig_redeemscript(
    redeemscript: &Script,
) -> Result<(PublicKey, PublicKey), ContractError> {
    let ms_rs_bytes = redeemscript.to_bytes();
    //TODO put these magic numbers in consts, PUBKEY1_OFFSET maybe
    let pubkey1 = PublicKey::from_slice(&ms_rs_bytes[2..35])?;
    let pubkey2 = PublicKey::from_slice(&ms_rs_bytes[36..69])?;
    Ok((pubkey1, pubkey2))
}

/// Create a Contract Transaction for the "Sender" side of Coinswap.
/// The Sender gets the coins back via timelock.
/// Receiver gets the coins via hashlock.
pub fn create_senders_contract_tx(
    input: OutPoint,
    input_value: u64,
    contract_redeemscript: &ScriptBuf,
) -> Transaction {
    Transaction {
        input: vec![TxIn {
            previous_output: input,
            sequence: Sequence::ZERO,
            witness: Witness::new(),
            script_sig: ScriptBuf::new(),
        }],
        output: vec![TxOut {
            script_pubkey: redeemscript_to_scriptpubkey(&contract_redeemscript),
            // TODO: Mining fee for contract tx is hard coded here. Make it configurable.
            value: input_value - 1000,
        }],
        lock_time: LockTime::ZERO,
        version: 2,
    }
}

pub fn create_receivers_contract_tx(
    input: OutPoint,
    input_value: u64,
    contract_redeemscript: &ScriptBuf,
) -> Transaction {
    //exactly the same thing as senders contract for now, until collateral
    //inputs are implemented
    create_senders_contract_tx(input, input_value, contract_redeemscript)
}

pub fn is_contract_out_valid(
    contract_output: &TxOut,
    hashlock_pubkey: &PublicKey,
    timelock_pubkey: &PublicKey,
    hashvalue: &Hash160,
    locktime: &u16,
    minimum_locktime: &u16,
) -> Result<(), ContractError> {
    if minimum_locktime > locktime {
        return Err(ContractError::Protocol("locktime too short"));
    }

    let redeemscript_from_request =
        create_contract_redeemscript(hashlock_pubkey, timelock_pubkey, hashvalue, locktime);
    let contract_spk_from_request = redeemscript_to_scriptpubkey(&redeemscript_from_request);
    if contract_output.script_pubkey != contract_spk_from_request {
        return Err(ContractError::Protocol(
            "given transaction does not pay to requested contract",
        ));
    }
    Ok(())
}

pub fn validate_contract_tx(
    receivers_contract_tx: &Transaction,
    funding_outpoint: Option<&OutPoint>,
    contract_redeemscript: &ScriptBuf,
) -> Result<(), ContractError> {
    if receivers_contract_tx.input.len() != 1 || receivers_contract_tx.output.len() != 1 {
        return Err(ContractError::Protocol(
            "invalid number of inputs or outputs",
        ));
    }
    if funding_outpoint.is_some()
        && receivers_contract_tx.input[0].previous_output != *funding_outpoint.unwrap()
    {
        return Err(ContractError::Protocol("not spending the funding outpoint"));
    }
    if receivers_contract_tx.output[0].script_pubkey
        != redeemscript_to_scriptpubkey(&contract_redeemscript)
    {
        return Err(ContractError::Protocol("doesnt pay to requested contract"));
    }
    Ok(())
}

pub fn sign_contract_tx(
    contract_tx: &Transaction,
    multisig_redeemscript: &Script,
    funding_amount: u64,
    privkey: &SecretKey,
) -> Result<Signature, ContractError> {
    let input_index = 0;
    let sighash = Message::from_slice(
        &SighashCache::new(contract_tx).segwit_signature_hash(
            input_index,
            multisig_redeemscript,
            funding_amount,
            EcdsaSighashType::All,
        )?[..],
    )?;
    let secp = Secp256k1::new();
    Ok(secp.sign_ecdsa(&sighash, privkey))
}

pub fn verify_contract_tx_sig(
    contract_tx: &Transaction,
    multisig_redeemscript: &Script,
    funding_amount: u64,
    pubkey: &PublicKey,
    sig: &Signature,
) -> Result<(), ContractError> {
    let input_index = 0;
    let sighash = Message::from_slice(
        &SighashCache::new(contract_tx).segwit_signature_hash(
            input_index,
            multisig_redeemscript,
            funding_amount,
            EcdsaSighashType::All,
        )?[..],
    )?;
    let secp = Secp256k1::new();
    Ok(secp.verify_ecdsa(&sighash, sig, &pubkey.inner)?)
}

#[cfg(test)]
mod test {
    use super::*;
    use bitcoin::{
        consensus::encode::deserialize,
        hashes::hex::FromHex,
        secp256k1::{
            self,
            rand::{random, thread_rng, Rng},
        },
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
        let maker_key_computed = calculate_pubkey_from_nonce(&pubkey, &nonce).unwrap();
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
        let (pubkey_derived, nonce) = derive_maker_pubkey_and_nonce(&pubkey_org).unwrap();
        let nonce_point = secp256k1::PublicKey::from_secret_key(&secp, &nonce);
        let expected_derivation = PublicKey {
            compressed: true,
            inner: pubkey_org.inner.combine(&nonce_point).unwrap(),
        };
        assert_eq!(pubkey_derived, expected_derivation);
    }

    #[test]
    fn test_contract_script_generation() {
        // create a random hashvalue
        let hashvalue = Hash160::from_slice(&thread_rng().gen::<[u8; 20]>()).unwrap();

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
            create_contract_redeemscript(&pub_hashlock, &pub_timelock, &hashvalue, &locktime);

        // Get the byte encoded locktime for script
        let locktime_bytecode = Builder::new().push_int(locktime as i64).into_script();

        // Below is hand made script string that should be expected
        let expected = "827ca914".to_owned()
            + &hashvalue.to_string()
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
        assert_eq!(read_contract_locktime(&contract_script).unwrap(), locktime);
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
        let multisig_redeemscript = ScriptBuf::from(Vec::from_hex("5221032e58afe51f9ed8ad3cc7897f634d881fdbe49a81564629ded8156bebd2ffd1af21039b6347398505f5ec93826dc61c19f47c66c0283ee9be980e29ce325a0f4679ef52ae").unwrap());
        let another_script = ScriptBuf::from(Vec::from_hex("020000000156944c5d3f98413ef45cf54545538103cc9f298e0575820ad3591376e2e0f65d2a0000000000000000014871000000000000220020dad1b452caf4a0f26aecf1cc43aaae9b903a043c34f75ad9a36c86317b22236800000000").unwrap());

        let multi_script_pubkey = redeemscript_to_scriptpubkey(&multisig_redeemscript);
        let another_script_pubkey = redeemscript_to_scriptpubkey(&another_script);

        // Create the funding transaction
        let funding_tx = Transaction {
            input: vec![TxIn {
                previous_output: OutPoint::from_str(
                    "5df6e0e2761359d30a8275058e299fcc0381534545f55cf43e41983f5d4c9456:42",
                )
                .unwrap(),
                sequence: Sequence::ZERO,
                witness: Witness::new(),
                script_sig: ScriptBuf::new(),
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
            lock_time: LockTime::ZERO,
            version: 2,
        };

        let funding_info = FundingTxInfo {
            funding_tx,
            multisig_redeemscript,
            funding_tx_merkleproof: String::new(),
            multisig_nonce: SecretKey::new(&mut thread_rng()),
            contract_redeemscript: ScriptBuf::new(),
            hashlock_nonce: SecretKey::new(&mut thread_rng()),
        };

        // Check the correct 2of2 multisig output is extracted from funding tx
        assert_eq!(1u32, find_funding_output_index(&funding_info).unwrap());
    }

    #[test]
    fn test_contract_tx_miscellaneous() {
        let contract_script = ScriptBuf::from(Vec::from_hex(
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
        let locktime = read_contract_locktime(&contract_script).unwrap();
        let (pub1, pub2) = read_pubkeys_from_contract_reedimscript(&contract_script).unwrap();

        // Validates if contract outpoint is correct
        assert!(is_contract_out_valid(
            &contract_tx.output[0],
            &pub1,
            &pub2,
            &hashvalue,
            &locktime,
            &2
        )
        .is_ok());

        // Validate if the contract transaction is spending correctl utxo
        assert!(validate_contract_tx(&contract_tx, Some(&spending_utxo), &contract_script).is_ok());

        // Error Cases---------------------------------------------
        // Check validation against wrong spending outpoint
        if let ContractError::Protocol(message) = validate_contract_tx(
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
            sequence: Sequence::ZERO,
            witness: Witness::new(),
            script_sig: ScriptBuf::new(),
        });
        // Verify validation fails
        if let ContractError::Protocol(message) =
            validate_contract_tx(&contract_tx_err1, Some(&spending_utxo), &contract_script)
                .unwrap_err()
        {
            assert_eq!(message, "invalid number of inputs or outputs");
        } else {
            panic!();
        }

        // Change contract transaction to pay into wrong output
        let mut contract_tx_err2 = contract_tx.clone();
        let multisig_redeemscript = ScriptBuf::from(Vec::from_hex("5221032e58afe51f9ed8ad3cc7897f634d881fdbe49a81564629ded8156bebd2ffd1af21039b6347398505f5ec93826dc61c19f47c66c0283ee9be980e29ce325a0f4679ef52ae").unwrap());
        let multi_script_pubkey = redeemscript_to_scriptpubkey(&multisig_redeemscript);
        contract_tx_err2.output[0] = TxOut {
            script_pubkey: multi_script_pubkey,
            value: 3000,
        };
        // Verify validation fails
        if let ContractError::Protocol(message) =
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
                sequence: Sequence::ZERO,
                witness: Witness::new(),
                script_sig: ScriptBuf::new(),
            }],
            output: vec![TxOut {
                script_pubkey: funding_spk,
                value: 2000,
            }],
            lock_time: LockTime::ZERO,
            version: 2,
        };

        // Create the contract transaction spending the funding outpoint
        let funding_outpoint = OutPoint::new(funding_tx.txid(), 0);

        let contract_script = ScriptBuf::from(Vec::from_hex("827ca914cdccf6695323f22d061a58c398deba38bba47148876321032e58afe51f9ed8ad3cc7897f634d881fdbe49a81564629ded8156bebd2ffd1af0120516721039b6347398505f5ec93826dc61c19f47c66c0283ee9be980e29ce325a0f4679ef000812dabb690fe0fd3768b2757b88ac").unwrap());

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
            &priv_1.inner,
        )
        .unwrap();

        assert!(verify_contract_tx_sig(
            &contract_tx,
            &funding_outpoint_script,
            funding_tx.output[0].value,
            &pub1,
            &sig1
        )
        .is_ok());

        // priv2 signs the contract and verify
        let sig2 = sign_contract_tx(
            &contract_tx,
            &funding_outpoint_script,
            funding_tx.output[0].value,
            &priv_2.inner,
        )
        .unwrap();

        assert!(verify_contract_tx_sig(
            &contract_tx,
            &funding_outpoint_script,
            funding_tx.output[0].value,
            &pub2,
            &sig2
        )
        .is_ok());
    }
}

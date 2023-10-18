//! Various Utility and Helper functions used in both Taker and Maker protocols.

use std::sync::Once;

use bitcoin::{
    address::{WitnessProgram, WitnessVersion},
    hashes::Hash,
    script::PushBytesBuf,
    secp256k1::{
        rand::{rngs::OsRng, RngCore},
        Secp256k1, SecretKey,
    },
    Network, PublicKey, ScriptBuf,
};

use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::tcp::{ReadHalf, WriteHalf},
};

use crate::{
    error::NetError,
    protocol::{
        contract::derive_maker_pubkey_and_nonce,
        messages::{MakerToTakerMessage, MultisigPrivkey},
    },
    wallet::{SwapCoin, WalletError},
};

pub fn str_to_bitcoin_network(net_str: &str) -> Network {
    match net_str {
        "main" => Network::Bitcoin,
        "test" => Network::Testnet,
        "signet" => Network::Signet,
        "regtest" => Network::Regtest,
        _ => panic!("unknown network: {}", net_str),
    }
}

/// Setup function that will only run once, even if called multiple times.
pub fn setup_logger() {
    Once::new().call_once(|| {
        env_logger::Builder::from_env(
            env_logger::Env::default()
                .default_filter_or("coinswap=info")
                .default_write_style_or("always"),
        )
        .init();
    });
}

/// Can send both Taker and Maker messages
pub async fn send_message(
    socket_writer: &mut WriteHalf<'_>,
    message: &impl serde::Serialize,
) -> Result<(), NetError> {
    let mut message_bytes = serde_json::to_vec(message).map_err(|e| std::io::Error::from(e))?;
    message_bytes.push(b'\n');
    socket_writer.write_all(&message_bytes).await?;
    Ok(())
}

/// Read a Maker Message
pub async fn read_message(
    reader: &mut BufReader<ReadHalf<'_>>,
) -> Result<MakerToTakerMessage, NetError> {
    let mut line = String::new();
    let n = reader.read_line(&mut line).await?;
    if n == 0 {
        return Err(NetError::ReachedEOF);
    }
    let message: MakerToTakerMessage = serde_json::from_str(&line)?;
    log::debug!("<== {:#?}", message);
    Ok(message)
}

/// Apply the maker's privatekey to swapcoins, and check it's the correct privkey for corresponding pubkey.
pub fn check_and_apply_maker_private_keys<S: SwapCoin>(
    swapcoins: &mut Vec<S>,
    swapcoin_private_keys: &[MultisigPrivkey],
) -> Result<(), WalletError> {
    for (swapcoin, swapcoin_private_key) in swapcoins.iter_mut().zip(swapcoin_private_keys.iter()) {
        swapcoin.apply_privkey(swapcoin_private_key.key)?;
    }
    Ok(())
}

/// Generate The Maker's Multisig and HashLock keys and respective nonce values.
/// Nonce values are random integers and resulting Pubkeys are derived by tweaking the
/// Make's advertised Pubkey with these two nonces.
pub fn generate_maker_keys(
    tweakable_point: &PublicKey,
    count: u32,
) -> (
    Vec<PublicKey>,
    Vec<SecretKey>,
    Vec<PublicKey>,
    Vec<SecretKey>,
) {
    let (multisig_pubkeys, multisig_nonces): (Vec<_>, Vec<_>) = (0..count)
        .map(|_| derive_maker_pubkey_and_nonce(tweakable_point).unwrap())
        .unzip();
    let (hashlock_pubkeys, hashlock_nonces): (Vec<_>, Vec<_>) = (0..count)
        .map(|_| derive_maker_pubkey_and_nonce(tweakable_point).unwrap())
        .unzip();
    (
        multisig_pubkeys,
        multisig_nonces,
        hashlock_pubkeys,
        hashlock_nonces,
    )
}

pub fn convert_json_rpc_bitcoin_to_satoshis(amount: &Value) -> u64 {
    //to avoid floating point arithmetic, convert the bitcoin amount to
    //string with 8 decimal places, then remove the decimal point to
    //obtain the value in satoshi
    //this is necessary because the json rpc represents bitcoin values
    //as floats :(
    format!("{:.8}", amount.as_f64().unwrap())
        .replace('.', "")
        .parse::<u64>()
        .unwrap()
}
// returns None if not a hd descriptor (but possibly a swapcoin (multisig) descriptor instead)
pub fn get_hd_path_from_descriptor(descriptor: &str) -> Option<(&str, u32, i32)> {
    //e.g
    //"desc": "wpkh([a945b5ca/1/1]029b77637989868dcd502dbc07d6304dc2150301693ae84a60b379c3b696b289ad)#aq759em9",
    let open = descriptor.find('[');
    let close = descriptor.find(']');
    if open.is_none() || close.is_none() {
        //unexpected, so printing it to stdout
        println!("unknown descriptor = {}", descriptor);
        return None;
    }
    let path = &descriptor[open.unwrap() + 1..close.unwrap()];
    let path_chunks: Vec<&str> = path.split('/').collect();
    if path_chunks.len() != 3 {
        return None;
        //unexpected descriptor = wsh(multi(2,[f67b69a3]0245ddf535f08a04fd86d794b76f8e3949f27f7ae039b641bf277c6a4552b4c387,[dbcd3c6e]030f781e9d2a6d3a823cee56be2d062ed4269f5a6294b20cb8817eb540c641d9a2))#8f70vn2q
    }
    let addr_type = path_chunks[1].parse::<u32>();
    if addr_type.is_err() {
        log::debug!(target: "wallet", "unexpected address_type = {}", path);
        return None;
    }
    let index = path_chunks[2].parse::<i32>();
    if index.is_err() {
        return None;
    }
    Some((path_chunks[0], addr_type.unwrap(), index.unwrap()))
}

pub fn generate_keypair() -> (PublicKey, SecretKey) {
    let mut privkey = [0u8; 32];
    OsRng.fill_bytes(&mut privkey);
    let secp = Secp256k1::new();
    let privkey = SecretKey::from_slice(&privkey).unwrap();
    let pubkey = PublicKey {
        compressed: true,
        inner: bitcoin::secp256k1::PublicKey::from_secret_key(&secp, &privkey),
    };
    (pubkey, privkey)
}

/// Convert a redeemscript into p2wsh scriptpubkey.
pub fn redeemscript_to_scriptpubkey(redeemscript: &ScriptBuf) -> ScriptBuf {
    let witness_program = WitnessProgram::new(
        WitnessVersion::V0,
        PushBytesBuf::from(&redeemscript.wscript_hash().to_byte_array()),
    )
    .unwrap();
    //p2wsh address
    ScriptBuf::new_witness_program(&witness_program)
}

pub fn to_hex(bytes: &Vec<u8>) -> String {
    let hex_chars: Vec<char> = "0123456789abcdef".chars().collect();
    let mut hex_string = String::new();

    for &byte in bytes {
        let high_nibble = (byte >> 4) & 0xF;
        let low_nibble = byte & 0xF;
        hex_string.push(hex_chars[high_nibble as usize]);
        hex_string.push(hex_chars[low_nibble as usize]);
    }

    hex_string
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use bitcoin::{
        blockdata::{opcodes::all, script::Builder},
        secp256k1::{Scalar, Secp256k1},
        PubkeyHash, Txid,
    };

    use serde_json::json;

    use super::*;

    #[test]
    fn test_str_to_bitcoin_network_main() {
        let net_str = "main";
        let network = str_to_bitcoin_network(net_str);
        assert_eq!(network, Network::Bitcoin);
    }

    #[test]
    fn test_str_to_bitcoin_network_test() {
        let net_str = "test";
        let network = str_to_bitcoin_network(net_str);
        assert_eq!(network, Network::Testnet);
    }

    #[test]
    fn test_str_to_bitcoin_network_signet() {
        let net_str = "signet";
        let network = str_to_bitcoin_network(net_str);
        assert_eq!(network, Network::Signet)
    }

    #[test]
    fn test_str_to_bitcoin_network_regtest() {
        let net_str = "regtest";
        let network = str_to_bitcoin_network(net_str);
        assert_eq!(network, Network::Regtest)
    }

    #[test]
    #[should_panic]
    fn test_str_to_bitcoin_network_unknown() {
        let net_str = "unknown_network";
        str_to_bitcoin_network(net_str);
    }

    #[test]
    fn test_convert_json_rpc_bitcoin_to_satoshis() {
        // Test with an integer value
        let amount = json!(1);
        assert_eq!(convert_json_rpc_bitcoin_to_satoshis(&amount), 100_000_000);

        // Test with a very large value
        let amount = json!(12345678.12345678);
        assert_eq!(
            convert_json_rpc_bitcoin_to_satoshis(&amount),
            1_234_567_812_345_678
        );
    }
    #[test]
    fn test_to_hex() {
        let mut txid_test_vector = [
            vec![
                0x5A, 0x4E, 0xBF, 0x66, 0x82, 0x2B, 0x0B, 0x2D, 0x56, 0xBD, 0x9D, 0xC6, 0x4E, 0xCE,
                0x0B, 0xC3, 0x8E, 0xE7, 0x84, 0x4A, 0x23, 0xFF, 0x1D, 0x73, 0x20, 0xA8, 0x8C, 0x5F,
                0xDB, 0x2A, 0xD3, 0xE2,
            ],
            vec![
                0x6D, 0x69, 0x37, 0x2E, 0x3E, 0x59, 0x28, 0xA7, 0x3C, 0x98, 0x38, 0x18, 0xBD, 0x19,
                0x27, 0xE1, 0x90, 0x8F, 0x51, 0xA6, 0xC2, 0xCD, 0x32, 0x58, 0x98, 0xB3, 0xB4, 0x16,
                0x90, 0xD4, 0xFA, 0x7B,
            ],
        ];
        for i in txid_test_vector.iter_mut() {
            let txid1 = Txid::from_str(to_hex(i).as_str()).unwrap();
            i.reverse();
            let txid2 = Txid::from_slice(i).unwrap();
            assert_eq!(txid1, txid2)
        }
    }
    #[test]
    fn test_redeemscript_to_scriptpubkey_custom() {
        // Create a custom puzzle script
        let puzzle_script = Builder::new()
            .push_opcode(all::OP_ADD)
            .push_opcode(all::OP_PUSHNUM_2)
            .push_opcode(all::OP_EQUAL)
            .into_script();
        // Compare the redeemscript_to_scriptpubkey output with the expected value in hex
        assert_eq!(
            redeemscript_to_scriptpubkey(&puzzle_script).to_hex_string(),
            "0020c856c4dcad54542f34f0889a0c12acf2951f3104c85409d8b70387bbb2e95261"
        );
    }
    #[test]
    fn test_redeemscript_to_scriptpubkey_p2pkh() {
        let pubkeyhash = PubkeyHash::from_str("79fbfc3f34e7745860d76137da68f362380c606c").unwrap();
        let script = Builder::new()
            .push_opcode(all::OP_DUP)
            .push_opcode(all::OP_HASH160)
            .push_slice(pubkeyhash.to_byte_array())
            .push_opcode(all::OP_EQUALVERIFY)
            .push_opcode(all::OP_CHECKSIG)
            .into_script();
        assert_eq!(
            redeemscript_to_scriptpubkey(&script).to_hex_string(),
            "0020de4c0f5b48361619b1cf09d5615bc3a2603c412bf4fcbc9acecf6786c854b741"
        );
    }

    #[test]
    fn test_redeemscript_to_scriptpubkey_1of2musig() {
        let pubkey1 = PublicKey::from_str(
            "03cccac45f4521514187be4b5650ecb241d4d898aa41daa7c5384b2d8055fbb509",
        )
        .unwrap();
        let pubkey2 = PublicKey::from_str(
            "0316665712a0b90de0bcf7cac70d3fd3cfd102050e99b5cd41a55f2c92e1d9e6f5",
        )
        .unwrap();
        let script = Builder::new()
            .push_opcode(all::OP_PUSHNUM_1)
            .push_key(&pubkey1)
            .push_key(&pubkey2)
            .push_opcode(all::OP_PUSHNUM_2)
            .push_opcode(all::OP_CHECKMULTISIG)
            .into_script();
        assert_eq!(
            redeemscript_to_scriptpubkey(&script).to_hex_string(),
            "0020b5954ef36e6bd532c7e90f41927a3556b0fef6416695dbe50ff40c6a55a6232c"
        );
    }
    #[test]
    fn test_hd_path_from_descriptor() {
        assert_eq!(get_hd_path_from_descriptor("wpkh([a945b5ca/1/1]020b77637989868dcd502dbc07d6304dc2150301693ae84a60b379c3b696b289ad)#aq759em9"), Some(("a945b5ca", 1, 1)));
    }
    #[test]
    fn test_hd_path_from_descriptor_gets_none() {
        assert_eq!(get_hd_path_from_descriptor("wsh(multi(2,[f67b69a3]0245ddf535f08a04fd86d794b76f8e3949f27f7ae039b641bf277c6a4552b4c387,[dbcd3c6e]030f781e9d2a6d3a823cee56be2d062ed4269f5a6294b20cb8817eb540c641d9a2))#8f70vn2q"), None);
    }

    #[test]
    fn test_generate_maker_keys() {
        // generate_maker_keys: test that given a tweakable_point the return values satisfy the equation:
        // tweak_point * returned_nonce = returned_publickey
        let tweak_point = PublicKey::from_str(
            "032e58afe51f9ed8ad3cc7897f634d881fdbe49a81564629ded8156bebd2ffd1af",
        )
        .unwrap();
        let (multisig_pubkeys, multisig_nonces, hashlock_pubkeys, hashlock_nonces) =
            generate_maker_keys(&tweak_point, 1);
        // test returned multisg part
        let returned_nonce = multisig_nonces[0];
        let returned_pubkey = multisig_pubkeys[0];
        let secp = Secp256k1::new();
        let pubkey_secp = bitcoin::secp256k1::PublicKey::from_str(
            "032e58afe51f9ed8ad3cc7897f634d881fdbe49a81564629ded8156bebd2ffd1af",
        )
        .unwrap();
        let scalar_from_nonce: Scalar = Scalar::from(returned_nonce);
        let tweaked_pubkey = pubkey_secp
            .add_exp_tweak(&secp, &scalar_from_nonce)
            .unwrap();
        assert_eq!(returned_pubkey.to_string(), tweaked_pubkey.to_string());

        // test returned hashlock part
        let returned_nonce = hashlock_nonces[0];
        let returned_pubkey = hashlock_pubkeys[0];
        let scalar_from_nonce: Scalar = Scalar::from(returned_nonce);
        let tweaked_pubkey = pubkey_secp
            .add_exp_tweak(&secp, &scalar_from_nonce)
            .unwrap();
        assert_eq!(returned_pubkey.to_string(), tweaked_pubkey.to_string());
    }
}

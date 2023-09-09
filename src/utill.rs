//! Various Utility and Helper functions used in both Taker and Maker protocols.

use std::io::ErrorKind;

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
    error::TeleportError,
    protocol::{
        contract::derive_maker_pubkey_and_nonce,
        messages::{MakerToTakerMessage, MultisigPrivkey},
    },
    wallet::SwapCoin,
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

/// Can send both Taker and Maker messages
pub async fn send_message(
    socket_writer: &mut WriteHalf<'_>,
    message: &impl serde::Serialize,
) -> Result<(), TeleportError> {
    let mut message_bytes = serde_json::to_vec(message).map_err(|e| std::io::Error::from(e))?;
    message_bytes.push(b'\n');
    socket_writer.write_all(&message_bytes).await?;
    Ok(())
}

/// Read a Maker Message
pub async fn read_message(
    reader: &mut BufReader<ReadHalf<'_>>,
) -> Result<MakerToTakerMessage, TeleportError> {
    let mut line = String::new();
    let n = reader.read_line(&mut line).await?;
    if n == 0 {
        return Err(TeleportError::Network(Box::new(std::io::Error::new(
            ErrorKind::ConnectionReset,
            "EOF",
        ))));
    }
    let message: MakerToTakerMessage = match serde_json::from_str(&line) {
        Ok(r) => r,
        Err(_e) => return Err(TeleportError::Protocol("json parsing error")),
    };
    log::debug!("<== {:#?}", message);
    Ok(message)
}

/// Apply the maker's privatekey to swapcoins, and check it's the correct privkey for corresponding pubkey.
pub fn check_and_apply_maker_private_keys<S: SwapCoin>(
    swapcoins: &mut Vec<S>,
    swapcoin_private_keys: &[MultisigPrivkey],
) -> Result<(), TeleportError> {
    for (swapcoin, swapcoin_private_key) in swapcoins.iter_mut().zip(swapcoin_private_keys.iter()) {
        swapcoin
            .apply_privkey(swapcoin_private_key.key)
            .map_err(|_| TeleportError::Protocol("wrong privkey"))?;
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
        .replace(".", "")
        .parse::<u64>()
        .unwrap()
}

// returns None if not a hd descriptor (but possibly a swapcoin (multisig) descriptor instead)
pub fn get_hd_path_from_descriptor<'a>(descriptor: &'a str) -> Option<(&'a str, u32, i32)> {
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
    use bitcoin::blockdata::{opcodes::all, script::Builder};
    use serde_json::json;

    use super::*;

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
    fn test_to_hex_empty_bytes() {
        let bytes: Vec<u8> = Vec::new();
        assert_eq!(to_hex(&bytes), "");
    }

    #[test]
    fn test_to_hex_single_byte() {
        let bytes: Vec<u8> = vec![0xAB];
        assert_eq!(to_hex(&bytes), "ab");
    }

    #[test]
    fn test_to_hex_multiple_bytes() {
        let bytes: Vec<u8> = vec![0x12, 0x34, 0x56, 0xFF];
        assert_eq!(to_hex(&bytes), "123456ff");
    }

    #[test]
    fn test_redeemscript_to_scriptpubkey() {
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
}

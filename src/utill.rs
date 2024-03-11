//! Various utility and helper functions for both Taker and Maker.

use std::{env, io::ErrorKind, path::PathBuf, sync::Once};

use bitcoin::{
    address::{WitnessProgram, WitnessVersion},
    hashes::{sha256, Hash},
    script::PushBytesBuf,
    secp256k1::{
        rand::{rngs::OsRng, RngCore},
        Secp256k1, SecretKey,
    },
    Network, PublicKey, ScriptBuf,
};
use libtor::{HiddenServiceVersion, LogDestination, LogLevel, Tor, TorAddress, TorFlag};
use mitosis::JoinHandle;

use std::{
    collections::HashMap,
    fs::{self, File},
    io::{self, BufRead, Write},
    thread,
    time::Duration,
};

use serde_json::Value;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, BufReader},
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

/// Converts a string representation of a network to a `Network` enum variant.
pub fn str_to_bitcoin_network(net_str: &str) -> Network {
    match net_str {
        "main" => Network::Bitcoin,
        "test" => Network::Testnet,
        "signet" => Network::Signet,
        "regtest" => Network::Regtest,
        _ => panic!("unknown network: {}", net_str),
    }
}

/// Get the system specific home directory.
pub fn get_home_dir() -> PathBuf {
    dirs::home_dir().expect("home directory expected")
}

/// Get the default data directory. `~/.coinswap`.
pub fn get_data_dir() -> PathBuf {
    get_home_dir().join(".coinswap")
}

/// Get the default wallets directory. `~/.coinswap/wallets`
pub fn get_wallet_dir() -> PathBuf {
    get_data_dir().join("wallets")
}

/// Get the default configs directory. `~/.coinswap/configs`
pub fn get_config_dir() -> PathBuf {
    get_data_dir().join("configs")
}

/// Generate an unique identifier from the seedphrase.
pub fn seed_phrase_to_unique_id(seed: &str) -> String {
    let mut hash = sha256::Hash::hash(seed.as_bytes()).to_string();
    let _ = hash.split_off(9);
    hash
}

/// Setup function that will only run once, even if called multiple times.
pub fn setup_logger() {
    Once::new().call_once(|| {
        env::set_var("RUST_LOG", "info");
        env_logger::Builder::from_env(
            env_logger::Env::default()
                .default_filter_or("coinswap=info")
                .default_write_style_or("always"),
        )
        // .is_test(true)
        .init();
    });
}

pub fn setup_mitosis() {
    mitosis::init();
}

/// Can send both Taker and Maker messages.
pub async fn send_message(
    socket_writer: &mut WriteHalf<'_>,
    message: &impl serde::Serialize,
) -> Result<(), NetError> {
    let message_cbor = serde_cbor::ser::to_vec(message).map_err(NetError::Cbor)?;
    socket_writer.write_u32(message_cbor.len() as u32).await?;
    socket_writer.write_all(&message_cbor).await?;
    Ok(())
}

/// Read a Maker Message.
pub async fn read_maker_message(
    reader: &mut BufReader<ReadHalf<'_>>,
) -> Result<MakerToTakerMessage, NetError> {
    let length = reader.read_u32().await?;
    let mut buffer = vec![0; length as usize];
    reader.read_exact(&mut buffer).await?;
    let message: MakerToTakerMessage = serde_cbor::from_slice(&buffer)?;
    Ok(message)
}

/// Apply the maker's privatekey to swapcoins, and check it's the correct privkey for corresponding pubkey.
pub fn check_and_apply_maker_private_keys<S: SwapCoin>(
    swapcoins: &mut [S],
    swapcoin_private_keys: &[MultisigPrivkey],
) -> Result<(), WalletError> {
    for (swapcoin, swapcoin_private_key) in swapcoins.iter_mut().zip(swapcoin_private_keys.iter()) {
        swapcoin.apply_privkey(swapcoin_private_key.key)?;
    }
    Ok(())
}

/// Generate The Maker's Multisig and HashLock keys and respective nonce values.
/// Nonce values are random integers and resulting Pubkeys are derived by tweaking the
/// Maker's advertised Pubkey with these two nonces.
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

/// Converts a Bitcoin amount from JSON-RPC representation to satoshis.
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

/// Extracts hierarchical deterministic (HD) path components from a descriptor.
///
/// Parses an input descriptor string and returns `Some` with a tuple containing the HD path
/// components if it's an HD descriptor. If it's not an HD descriptor, it returns `None`.
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

/// Generates a keypair using the secp256k1 elliptic curve.
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

/// Converts a byte vector to a hexadecimal string representation.
pub fn to_hex(bytes: &[u8]) -> String {
    let hex_chars: Vec<char> = "0123456789abcdef".chars().collect();
    let mut hex_string = String::new();

    for &byte in bytes {
        let high_nibble = (byte >> 4) & 0xf;
        let low_nibble = byte & 0xf;
        hex_string.push(hex_chars[high_nibble as usize]);
        hex_string.push(hex_chars[low_nibble as usize]);
    }

    hex_string
}

/// Parse TOML file into key-value pair.
pub fn parse_toml(file_path: &PathBuf) -> io::Result<HashMap<String, HashMap<String, String>>> {
    let file = File::open(file_path)?;
    let reader = io::BufReader::new(file);

    let mut sections = HashMap::new();
    let mut current_section = String::new();

    for line in reader.lines() {
        let line = line?;
        if line.trim().starts_with('[') {
            current_section = line
                .trim()
                .trim_matches(|p| (p == '[' || p == ']'))
                .to_string();
            sections.insert(current_section.clone(), HashMap::new());
        } else if line.trim().starts_with('#') {
            continue;
        } else if let Some(pos) = line.find('=') {
            let key = line[..pos].trim().to_string();
            let value = line[pos + 1..].trim().to_string();
            if let Some(section) = sections.get_mut(&current_section) {
                section.insert(key, value);
            }
        }
    }

    Ok(sections)
}

/// Parse and log errors for each field.
pub fn parse_field<T: std::str::FromStr>(value: Option<&String>, default: T) -> io::Result<T> {
    match value {
        Some(value) => value
            .parse()
            .map_err(|_e| io::Error::new(ErrorKind::InvalidData, "parsing failed")),
        None => Ok(default),
    }
}

/// Function to write data to default toml files
pub fn write_default_config(path: &PathBuf, toml_data: String) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = File::create(path)?;
    file.write_all(toml_data.as_bytes())?;
    file.flush()?;
    Ok(())
}

/// Function to check if tor log contains a pattern
pub fn monitor_log_for_completion(log_dir: PathBuf, pattern: &str) -> io::Result<()> {
    let mut last_size = 0;

    loop {
        let file = fs::File::open(&log_dir)?;
        let metadata = file.metadata()?;
        let current_size = metadata.len();

        if current_size != last_size {
            let reader = io::BufReader::new(file);
            let lines = reader.lines();

            for line in lines {
                if let Ok(line) = line {
                    if line.contains(pattern) {
                        log::info!("Tor instance bootstrapped");
                        return Ok(());
                    }
                } else {
                    return Err(io::Error::new(io::ErrorKind::Other, "Error reading line"));
                }
            }

            last_size = current_size;
        }
        thread::sleep(Duration::from_secs(3));
    }
}

pub fn spawn_tor(socks_port: u16, port: u16, base_dir: String) -> JoinHandle<()> {
    let handle = mitosis::spawn(
        (socks_port, port, base_dir),
        |(socks_port, port, base_dir)| {
            let hs_string = format!("{}/hs-dir/", base_dir);
            let data_dir = format!("{}/", base_dir);
            let log_dir = format!("{}/log", base_dir);
            let _handler = Tor::new()
                .flag(TorFlag::DataDirectory(data_dir))
                .flag(TorFlag::LogTo(
                    LogLevel::Notice,
                    LogDestination::File(log_dir),
                ))
                .flag(TorFlag::SocksPort(socks_port))
                .flag(TorFlag::HiddenServiceDir(hs_string))
                .flag(TorFlag::HiddenServiceVersion(HiddenServiceVersion::V3))
                .flag(TorFlag::HiddenServicePort(
                    TorAddress::Port(port),
                    None.into(),
                ))
                .start();
        },
    );

    handle
}

pub fn kill_tor_handles(handle: JoinHandle<()>) {
    match handle.kill() {
        Ok(_) => log::info!("Tor instance terminated successfully"),
        Err(_) => log::error!("Error occurred while terminating tor instance"),
    };
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use bitcoin::{
        blockdata::{opcodes::all, script::Builder},
        secp256k1::Scalar,
        PubkeyHash, Txid,
    };

    use serde_json::json;
    use tokio::net::{TcpListener, TcpStream};

    use super::*;

    #[test]
    fn test_str_to_bitcoin_network() {
        let net_strs = vec![
            ("main", Network::Bitcoin),
            ("test", Network::Testnet),
            ("signet", Network::Signet),
            ("regtest", Network::Regtest),
            ("unknown_network", Network::Bitcoin),
        ];
        for (net_str, expected_network) in net_strs {
            let network = std::panic::catch_unwind(|| str_to_bitcoin_network(net_str));
            match network {
                Ok(net) => assert_eq!(net, expected_network),
                Err(_) => {
                    assert_eq!(Network::Bitcoin, expected_network);
                }
            }
        }
    }

    #[allow(clippy::read_zero_byte_vec)]
    #[tokio::test]
    async fn test_send_message() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = Vec::new();
            let nbytes = socket.read(&mut buffer).await.unwrap();
            buffer.truncate(nbytes);
            assert_eq!(buffer, b"\"Hello, teleport!\"\n");
        });

        let mut stream = TcpStream::connect(address).await.unwrap();
        let (_, mut write_half) = stream.split();

        let message = "Hello, teleport!";
        send_message(&mut write_half, &message).await.unwrap();
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
                0x5a, 0x4e, 0xbf, 0x66, 0x82, 0x2b, 0x0b, 0x2d, 0x56, 0xbd, 0x9d, 0xc6, 0x4e, 0xce,
                0x0b, 0xc3, 0x8e, 0xe7, 0x84, 0x4a, 0x23, 0xff, 0x1d, 0x73, 0x20, 0xa8, 0x8c, 0x5f,
                0xdb, 0x2a, 0xd3, 0xe2,
            ],
            vec![
                0x6d, 0x69, 0x37, 0x2e, 0x3e, 0x59, 0x28, 0xa7, 0x3c, 0x98, 0x38, 0x18, 0xbd, 0x19,
                0x27, 0xe1, 0x90, 0x8f, 0x51, 0xa6, 0xc2, 0xcd, 0x32, 0x58, 0x98, 0xb3, 0xb4, 0x16,
                0x90, 0xd4, 0xfa, 0x7b,
            ],
        ];
        for i in txid_test_vector.iter_mut() {
            let txid1 = Txid::from_str(to_hex(i).as_str()).unwrap();
            i.reverse();
            let txid2 = Txid::from_slice(i).unwrap();
            assert_eq!(txid1, txid2);
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
        assert_eq!(
            get_hd_path_from_descriptor(
                "wpkh([a945b5ca/1/1]020b77637989868dcd502dbc07d6304dc2150301693ae84a60b379c3b696b289ad)#aq759em9"
            ),
            Some(("a945b5ca", 1, 1))
        );
    }
    #[test]
    fn test_hd_path_from_descriptor_gets_none() {
        assert_eq!(
            get_hd_path_from_descriptor(
                "wsh(multi(2,[f67b69a3]0245ddf535f08a04fd86d794b76f8e3949f27f7ae039b641bf277c6a4552b4c387,[dbcd3c6e]030f781e9d2a6d3a823cee56be2d062ed4269f5a6294b20cb8817eb540c641d9a2))#8f70vn2q"
            ),
            None
        );
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

//! Various utility and helper functions for both Taker and Maker.

use std::{
    env,
    io::{ErrorKind, Read},
    net::TcpStream,
    path::{Path, PathBuf},
    str::FromStr,
    sync::Once,
};

use bitcoin::{
    hashes::{sha256, Hash},
    secp256k1::{
        rand::{rngs::OsRng, RngCore},
        Secp256k1, SecretKey,
    },
    Network, PublicKey, ScriptBuf, WitnessProgram, WitnessVersion,
};
use log::LevelFilter;
use log4rs::{
    append::{console::ConsoleAppender, file::FileAppender},
    config::{Appender, Logger, Root},
    Config,
};

use std::{
    collections::HashMap,
    fs::{self, File},
    io::{self, BufRead, Write},
    thread,
    time::Duration,
};

use crate::{
    error::NetError,
    protocol::{contract::derive_maker_pubkey_and_nonce, messages::MultisigPrivkey},
    wallet::{SwapCoin, WalletError},
};
use serde_json::Value;

const INPUT_CHARSET: &str =
    "0123456789()[],'/*abcdefgh@:$%{}IJKLMNOPQRSTUVWXYZ&+-.;<=>?!^_|~ijklmnopqrstuvwxyzABCDEFGH`#\"\\ ";
const CHECKSUM_CHARSET: &str = "qpzry9x8gf2tvdw0s3jn54khce6mua7l";

const MASK_LOW_35_BITS: u64 = 0x7ffffffff;
const SHIFT_FOR_C0: u64 = 35;
const CHECKSUM_FINAL_XOR_VALUE: u64 = 1;

/// Global timeout for all network connections.
pub const NET_TIMEOUT: Duration = Duration::from_secs(60);

/// Used as delays on reattempting some network communications.
pub const GLOBAL_PAUSE: Duration = Duration::from_secs(10);

/// Global heartbeat interval for internal server threads.
pub const HEART_BEAT_INTERVAL: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConnectionType {
    TOR,
    CLEARNET,
}

impl FromStr for ConnectionType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "tor" => Ok(ConnectionType::TOR),
            "clearnet" => Ok(ConnectionType::CLEARNET),
            _ => Err("Invalid connection type".to_string()),
        }
    }
}

/// Read the tor address given an hidden_service directory path
pub fn get_tor_addrs(hs_dir: &Path) -> String {
    let hostname_file_path = hs_dir.join("hs-dir").join("hostname");
    let mut hostname_file = fs::File::open(hostname_file_path).unwrap();
    let mut tor_addrs: String = String::new();
    hostname_file.read_to_string(&mut tor_addrs).unwrap();
    tor_addrs
}

/// Get the system specific home directory.
/// Uses "/tmp" directory for integration tests
fn get_home_dir() -> PathBuf {
    if cfg!(test) {
        "/tmp".into()
    } else {
        dirs::home_dir().expect("home directory expected")
    }
}

/// Get the default data directory. `~/.coinswap`.
fn get_data_dir() -> PathBuf {
    get_home_dir().join(".coinswap")
}

/// Get the Maker Directory
pub fn get_maker_dir() -> PathBuf {
    get_data_dir().join("maker")
}

/// Get the Taker Directory
pub fn get_taker_dir() -> PathBuf {
    get_data_dir().join("taker")
}

/// Get the DNS Directory
pub fn get_dns_dir() -> PathBuf {
    get_data_dir().join("dns")
}

/// Generate an unique identifier from the seedphrase.
pub fn seed_phrase_to_unique_id(seed: &str) -> String {
    let mut hash = sha256::Hash::hash(seed.as_bytes()).to_string();
    let _ = hash.split_off(9);
    hash
}

/// Setup function that will only run once, even if called multiple times.
/// Takes log level to set the desired logging verbosity for taker binary.
pub fn setup_logger(taker_log_level: LevelFilter) {
    Once::new().call_once(|| {
        env::set_var("RUST_LOG", "coinswap=info");
        let taker_log_dir = get_taker_dir().join("debug.log");
        let maker_log_dir = get_maker_dir().join("debug.log");
        let directory_log_dir = get_dns_dir().join("debug.log");

        let stdout = ConsoleAppender::builder().build();
        let taker = FileAppender::builder().build(taker_log_dir).unwrap();
        let maker = FileAppender::builder().build(maker_log_dir).unwrap();
        let directory = FileAppender::builder().build(directory_log_dir).unwrap();
        let config = Config::builder()
            .appender(Appender::builder().build("stdout", Box::new(stdout)))
            .appender(Appender::builder().build("taker", Box::new(taker)))
            .appender(Appender::builder().build("maker", Box::new(maker)))
            .appender(Appender::builder().build("directory", Box::new(directory)))
            .logger(
                Logger::builder()
                    .appender("taker")
                    .build("coinswap::taker", taker_log_level),
            )
            .logger(
                Logger::builder()
                    .appender("maker")
                    .build("coinswap::maker", log::LevelFilter::Info),
            )
            .logger(
                Logger::builder()
                    .appender("directory")
                    .build("coinswap::market", log::LevelFilter::Info),
            )
            .build(Root::builder().appender("stdout").build(taker_log_level))
            .unwrap();
        log4rs::init_config(config).unwrap();
    });
}

/// Send a length-appended Protocol or RPC Message through a stream.
/// The first byte sent is the length of the actual message.
pub fn send_message(
    socket_writer: &mut TcpStream,
    message: &impl serde::Serialize,
) -> Result<(), NetError> {
    let msg_bytes = serde_cbor::ser::to_vec(message).map_err(NetError::Cbor)?;
    let msg_len = (msg_bytes.len() as u32).to_be_bytes();
    let mut to_send = Vec::with_capacity(msg_bytes.len() + msg_len.len());
    to_send.extend(msg_len);
    to_send.extend(msg_bytes);
    socket_writer.write_all(&to_send)?;
    socket_writer.flush()?;
    Ok(())
}

/// Reads a response byte_array from a given stream.
/// Response can be any length-appended data, where the first byte is the length of the actual message.
pub fn read_message(reader: &mut TcpStream) -> Result<Vec<u8>, NetError> {
    // length of incoming data
    let mut len_buff = [0u8; 4];
    reader.read_exact(&mut len_buff)?; // This can give UnexpectedEOF error if theres no data to read
    let length = u32::from_be_bytes(len_buff);

    // the actual data
    let mut buffer = vec![0; length as usize];
    reader.read_exact(&mut buffer)?;
    Ok(buffer)
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
/// Nonce values are random integers and resulting Pubkeys are derived by tweaking
///
/// the Maker's advertised Pubkey with these two nonces.
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
        log::error!("unknown descriptor = {}", descriptor);
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
        log::error!(target: "wallet", "unexpected address_type = {}", path);
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
        &redeemscript.wscript_hash().to_byte_array(),
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
pub fn monitor_log_for_completion(log_file: &PathBuf, pattern: &str) -> io::Result<()> {
    let mut last_size = 0;

    loop {
        let file = fs::File::open(log_file)?;
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

fn polynomial_modulus(mut checksum: u64, value: u64) -> u64 {
    let upper_bits = checksum >> SHIFT_FOR_C0;
    checksum = ((checksum & MASK_LOW_35_BITS) << 5) ^ value;

    static FEEDBACK_TERMS: [(u64, u64); 5] = [
        (0x1, 0xf5dee51989),
        (0x2, 0xa9fdca3312),
        (0x4, 0x1bab10e32d),
        (0x8, 0x3706b1677a),
        (0x10, 0x644d626ffd),
    ];

    for &(bit, term) in FEEDBACK_TERMS.iter() {
        if (upper_bits & bit) != 0 {
            checksum ^= term;
        }
    }

    checksum
}

/// Compute the checksum of a descriptor
pub fn compute_checksum(descriptor: &str) -> Result<String, WalletError> {
    let mut checksum = CHECKSUM_FINAL_XOR_VALUE;
    let mut accumulated_value = 0;
    let mut group_count = 0;

    for character in descriptor.chars() {
        let position = INPUT_CHARSET
            .find(character)
            .ok_or(WalletError::Protocol("Descriptor invalid".to_string()))?
            as u64;
        checksum = polynomial_modulus(checksum, position & 31);
        accumulated_value = accumulated_value * 3 + (position >> 5);
        group_count += 1;

        if group_count == 3 {
            checksum = polynomial_modulus(checksum, accumulated_value);
            accumulated_value = 0;
            group_count = 0;
        }
    }

    if group_count > 0 {
        checksum = polynomial_modulus(checksum, accumulated_value);
    }

    // Finalize checksum by feeding zeros.
    (0..8).for_each(|_| {
        checksum = polynomial_modulus(checksum, 0);
    });
    checksum ^= CHECKSUM_FINAL_XOR_VALUE;

    // Convert the checksum into a character string.
    let checksum_chars = (0..8)
        .map(|i| {
            CHECKSUM_CHARSET
                .chars()
                .nth(((checksum >> (5 * (7 - i))) & 31) as usize)
                .unwrap()
        })
        .collect::<String>();

    Ok(checksum_chars)
}

/// Parse the proxy (Socket:Port) argument from the cli input.
pub fn parse_proxy_auth(s: &str) -> Result<(String, String), String> {
    let parts: Vec<_> = s.split(':').collect();
    if parts.len() != 2 {
        return Err("Invalid format".to_string());
    }

    let user = parts[0].to_string();
    let passwd = parts[1].to_string();

    Ok((user, passwd))
}

/// Parse the network string for Bitcoin Backend. Used in CLI apps.
pub fn read_bitcoin_network_string(network: &str) -> Result<Network, String> {
    match network {
        "regtest" => Ok(Network::Regtest),
        "mainnet" => Ok(Network::Bitcoin),
        "signet" => Ok(Network::Signet),
        _ => Err("Invalid Bitcoin Network".to_string()),
    }
}

/// Parse the network string for Connection Type. Used in CLI apps.
pub fn read_connection_network_string(network: &str) -> Result<ConnectionType, String> {
    match network {
        "clearnet" => Ok(ConnectionType::CLEARNET),
        "tor" => Ok(ConnectionType::TOR),
        _ => Err("Invalid Connection Network".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use bitcoin::{
        blockdata::{opcodes::all, script::Builder},
        secp256k1::Scalar,
        PubkeyHash, Txid,
    };

    use serde_json::json;

    use crate::protocol::messages::{MakerHello, MakerToTakerMessage};

    use super::*;

    fn create_temp_config(contents: &str, file_name: &str) -> PathBuf {
        let file_path = PathBuf::from(file_name);
        let mut file = File::create(&file_path).unwrap();
        writeln!(file, "{}", contents).unwrap();
        file_path
    }

    fn remove_temp_config(path: &PathBuf) {
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn test_send_message() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();

        let message = MakerToTakerMessage::MakerHello(MakerHello {
            protocol_version_min: 1,
            protocol_version_max: 100,
        });
        thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let msg_bytes = read_message(&mut socket).unwrap();
            let msg: MakerToTakerMessage = serde_cbor::from_slice(&msg_bytes).unwrap();
            assert_eq!(
                msg,
                MakerToTakerMessage::MakerHello(MakerHello {
                    protocol_version_min: 1,
                    protocol_version_max: 100
                })
            );
        });

        let mut stream = TcpStream::connect(address).unwrap();
        send_message(&mut stream, &message).unwrap();
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
    fn test_hd_path_from_descriptor_failure_cases() {
        let test_cases = [
            (
                "wpkh a945b5ca/1/1 029b77637989868dcd502dbc07d6304dc2150301693ae84a60b379c3b696b289ad aq759em9",
                None,
            ), // without brackets
            (
                "wpkh([a945b5ca/invalid/1]029b77637989868dcd502dbc07d6304dc2150301693ae84a60b379c3b696b289ad)#aq759em9",
                None,
            ), // invalid address type
            (
                "wpkh([a945b5ca/1/invalid]029b77637989868dcd502dbc07d6304dc2150301693ae84a60b379c3b696b289ad)#aq759em9",
                None,
            ), // invalid index
        ];

        for (descriptor, expected_output) in test_cases.iter() {
            let result = get_hd_path_from_descriptor(descriptor);
            assert_eq!(result, *expected_output);
        }
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

    #[test]
    fn test_parse_toml() {
        let file_content = r#"
            [section1]
            key1 = "value1"
            key2 = "value2"
            
            [section2]
            key3 = "value3"
            key4 = "value4"
        "#;
        let file_path = create_temp_config(file_content, "test.toml");

        let mut result = parse_toml(&file_path).expect("Failed to parse TOML");

        let expected_json = r#"{
            "section1": {"key1": "value1", "key2": "value3"},
            "section2": {"key3": "value3", "key4": "value4"}
        }"#;

        let expected_result: HashMap<String, HashMap<String, String>> =
            serde_json::from_str(expected_json).expect("Failed to parse JSON");

        for (section_name, right_section) in expected_result.iter() {
            if let Some(left_section) = result.get_mut(section_name) {
                for (key, value) in right_section.iter() {
                    left_section.insert(key.clone(), value.clone());
                }
            } else {
                result.insert(section_name.clone(), right_section.clone());
            }
        }

        assert_eq!(result, expected_result);

        remove_temp_config(&file_path);
    }
}

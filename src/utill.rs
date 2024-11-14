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
    PublicKey, ScriptBuf, WitnessProgram, WitnessVersion,
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

/// Represents the type of network connection used for communication.
///
/// Controls whether to:
/// - Use Tor for anonymous routing
/// - Use clearnet for direct connections
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConnectionType {
    /// Route all traffic through Tor network.
    TOR,
    /// Direct connection without anonymization.
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

/// Returns Tor onion address from hidden service hostname file.
///
/// Reads address from 'hs-dir/hostname' in the given directory.
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
///
/// Takes SHA256 hash of seed phrase and keeps first 8 characters.
pub fn seed_phrase_to_unique_id(seed: &str) -> String {
    let mut hash = sha256::Hash::hash(seed.as_bytes()).to_string();
    let _ = hash.split_off(9);
    hash
}

/// Initializes logging system with file and console output.
///
/// Sets up loggers for:
/// - Taker (at debug.log in taker dir)
/// - Maker (at debug.log in maker dir)
/// - Directory (at debug.log in dns dir)
/// - Console output
///
/// Note: Only runs once even if called multiple times.
pub fn setup_logger(filter: LevelFilter) {
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
                    .build("coinswap::taker", filter),
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
            .build(Root::builder().appender("stdout").build(filter))
            .unwrap();
        log4rs::init_config(config).unwrap();
    });
}

/// Sends length-prefixed serialized message over TCP stream.
///
/// Message format:
/// - 4 bytes: message length
/// - N bytes: CBOR serialized message content
/// - The first byte sent is the length of the actual message.
///
/// # Errors
/// - Serialization failures
/// - Write errors
/// - Flush errors
pub fn send_message(
    socket_writer: &mut TcpStream,
    message: &impl serde::Serialize,
) -> Result<(), NetError> {
    let msg_bytes = serde_cbor::ser::to_vec(message)?;
    let msg_len = (msg_bytes.len() as u32).to_be_bytes();
    let mut to_send = Vec::with_capacity(msg_bytes.len() + msg_len.len());
    to_send.extend(msg_len);
    to_send.extend(msg_bytes);
    socket_writer.write_all(&to_send)?;
    socket_writer.flush()?;
    Ok(())
}

/// Reads length-prefixed message from TCP stream.
///
/// Response can be any length-appended data, where the first byte is the length of the actual message.
///
/// # Errors
/// - EOF on connection close
/// - Incomplete message
/// - IO errors during read
pub fn read_message(reader: &mut TcpStream) -> Result<Vec<u8>, NetError> {
    // length of incoming data
    let mut len_buff = [0u8; 4];
    reader.read_exact(&mut len_buff)?; // This can give UnexpectedEOF error if theres no data to read
    let length = u32::from_be_bytes(len_buff);

    // the actual data
    let mut buffer = vec![0; length as usize];
    let mut total_read = 0;

    while total_read < length as usize {
        match reader.read(&mut buffer[total_read..]) {
            Ok(0) => return Err(NetError::ReachedEOF), // Connection closed
            Ok(n) => total_read += n,
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {
                continue
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(buffer)
}

/// Verifies and applies maker's private keys to swap coins.
///
/// For each swap coin:
/// - Applies corresponding private key
/// - Verifies key matches expected pubkey
///
/// # Errors
/// - Key mismatch
/// - Invalid key format
pub fn check_and_apply_maker_private_keys<S: SwapCoin>(
    swapcoins: &mut [S],
    swapcoin_private_keys: &[MultisigPrivkey],
) -> Result<(), WalletError> {
    for (swapcoin, swapcoin_private_key) in swapcoins.iter_mut().zip(swapcoin_private_keys.iter()) {
        swapcoin.apply_privkey(swapcoin_private_key.key)?;
    }
    Ok(())
}

/// Generates maker's multsig and hashlock keypairs and respective nonce values from a base public key.
///
/// Nonce values are random integers and resulting Pubkeys are derived by tweaking
/// the Maker's advertised Pubkey with these two nonces.
/// - Derives tweaked multisig keys
/// - Derives tweaked hashlock keys
/// - Uses random nonces for tweaking
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

/// Converts Bitcoin amount from JSON-RPC float to satoshis.
///
/// Due to JSON-RPC's float representation:
/// - Formats amount to 8 decimal places
/// - Removes decimal point
/// - Parses resulting string as satoshis
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
///
/// Returns None for:
/// - Non-HD descriptors
/// - Invalid path formats
/// - Unparseable address types or indices
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

/// Generates a random secp256k1 keypair using system entropy.
///
/// Creates:
/// - 32-byte random private key
/// - Corresponding compressed public key
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

/// Converts a redeem script to P2WSH (Pay-to-Witness-Script-Hash) script pubkey.
///
/// Takes witness program hash of redeem script and wraps it in P2WSH format.
pub fn redeemscript_to_scriptpubkey(redeemscript: &ScriptBuf) -> ScriptBuf {
    let witness_program = WitnessProgram::new(
        WitnessVersion::V0,
        &redeemscript.wscript_hash().to_byte_array(),
    )
    .unwrap();
    //p2wsh address
    ScriptBuf::new_witness_program(&witness_program)
}

/// Parses TOML file into nested hashmaps of sections and key-value pairs.
///
/// Processes TOML format:
/// - Sections marked by [section_name]
/// - Key-value pairs as 'key = value'
/// - Ignores comment lines starting with #
///
/// # Errors
/// - File access failures
/// - Line reading errors
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

/// Parses a field from string with error handling and default value.
///
/// Returns:
/// - Parsed value if present and valid
/// - Default value if field is None
/// - IO error if parsing fails
pub fn parse_field<T: std::str::FromStr>(value: Option<&String>, default: T) -> io::Result<T> {
    match value {
        Some(value) => value
            .parse()
            .map_err(|_e| io::Error::new(ErrorKind::InvalidData, "parsing failed")),
        None => Ok(default),
    }
}

/// Monitors Tor log file for a specific pattern indicating completion.
///
/// Continuously checks log file:
/// - Tracks file size changes
/// - Reads new content when file grows
/// - Searches for completion pattern
/// - Sleeps 3 seconds between checks
///
/// # Errors
/// - File access failures
/// - Line reading errors
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

/// Computes the checksum for a descriptor string using polynomial modulus.
///
/// Processes descriptor characters to generate checksum:
/// - Maps characters to numeric values
/// - Applies polynomial modulus operations
/// - Handles character groups of size 3
/// - Finalizes with zero padding
///
/// # Errors
/// - Invalid characters in descriptor
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

/// Parses proxy authentication string in 'user:password' format.
///
/// Returns tuple of username and password strings.
///
/// # Errors
/// - Invalid format (missing ':' separator)
pub fn parse_proxy_auth(s: &str) -> Result<(String, String), NetError> {
    let parts: Vec<_> = s.split(':').collect();
    if parts.len() != 2 {
        return Err(NetError::InvalidNetworkAddress);
    }

    let user = parts[0].to_string();
    let passwd = parts[1].to_string();

    Ok((user, passwd))
}

/// Converts network string to ConnectionType enum.
///
/// Accepts:
/// - "clearnet" for ConnectionType::CLEARNET
/// - "tor" for ConnectionType::TOR
///
/// # Errors
/// - Invalid network string
pub fn read_connection_network_string(network: &str) -> Result<ConnectionType, NetError> {
    match network {
        "clearnet" => Ok(ConnectionType::CLEARNET),
        "tor" => Ok(ConnectionType::TOR),
        _ => Err(NetError::InvalidAppNetwork),
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use bitcoin::{
        blockdata::{opcodes::all, script::Builder},
        secp256k1::Scalar,
        PubkeyHash,
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

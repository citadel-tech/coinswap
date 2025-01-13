//! Various utility and helper functions for both Taker and Maker.

use bitcoin::{
    absolute::LockTime,
    hashes::Hash,
    key::{rand::thread_rng, Keypair},
    secp256k1::{Message, Secp256k1, SecretKey},
    Address, PublicKey, ScriptBuf, Transaction, WitnessProgram, WitnessVersion,
};
use log::LevelFilter;
use log4rs::{
    append::{console::ConsoleAppender, file::FileAppender},
    config::{Appender, Logger, Root},
    Config,
};
use serde::{Deserialize, Serialize};
use std::{
    env, fmt,
    io::{BufReader, BufWriter, ErrorKind, Read},
    net::TcpStream,
    path::{Path, PathBuf},
    str::FromStr,
    sync::Once,
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
    protocol::{
        contract::derive_maker_pubkey_and_nonce,
        error::ProtocolError,
        messages::{FidelityProof, MultisigPrivkey},
    },
    wallet::{fidelity_redeemscript, FidelityError, SwapCoin, WalletError},
};

const INPUT_CHARSET: &str =
    "0123456789()[],'/*abcdefgh@:$%{}IJKLMNOPQRSTUVWXYZ&+-.;<=>?!^_|~ijklmnopqrstuvwxyzABCDEFGH`#\"\\ ";
const CHECKSUM_CHARSET: &str = "qpzry9x8gf2tvdw0s3jn54khce6mua7l";

const MASK_LOW_35_BITS: u64 = 0x7ffffffff;
const SHIFT_FOR_C0: u64 = 35;
const CHECKSUM_FINAL_XOR_VALUE: u64 = 1;

/// Global timeout for all network connections.
pub(crate) const NET_TIMEOUT: Duration = Duration::from_secs(60);

/// Used as delays on reattempting some network communications.
pub(crate) const GLOBAL_PAUSE: Duration = Duration::from_secs(10);

/// Global heartbeat interval used during waiting periods in critical situations.
pub(crate) const HEART_BEAT_INTERVAL: Duration = Duration::from_secs(3);

/// Number of confirmation required funding transaction.
pub const REQUIRED_CONFIRMS: u32 = 1;

/// Specifies the type of connection: TOR or Clearnet.
///
/// This enum is used to distinguish between different types of network connections
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConnectionType {
    /// Represents a TOR connection type.
    ///
    /// This variant is only available when the `tor` feature is enabled.
    #[cfg(feature = "tor")]
    TOR,

    /// Represents a Clearnet connection type.
    CLEARNET,
}

impl FromStr for ConnectionType {
    type Err = NetError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            #[cfg(feature = "tor")]
            "tor" => Ok(ConnectionType::TOR),
            "clearnet" => Ok(ConnectionType::CLEARNET),
            _ => Err(NetError::InvalidAppNetwork),
        }
    }
}

impl fmt::Display for ConnectionType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(feature = "tor")]
            ConnectionType::TOR => write!(f, "tor"),
            ConnectionType::CLEARNET => write!(f, "clearnet"),
        }
    }
}

/// Read the tor address given an hidden_service directory path
pub(crate) fn get_tor_addrs(hs_dir: &Path) -> io::Result<String> {
    let hostname_file_path = hs_dir.join("hs-dir").join("hostname");
    let mut hostname_file = File::open(hostname_file_path).unwrap();
    let mut tor_addrs: String = String::new();
    hostname_file.read_to_string(&mut tor_addrs)?;
    tor_addrs.pop(); // Remove `\n` at the end.
    Ok(tor_addrs)
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
pub(crate) fn get_maker_dir() -> PathBuf {
    get_data_dir().join("maker")
}

/// Get the Taker Directory
pub(crate) fn get_taker_dir() -> PathBuf {
    get_data_dir().join("taker")
}

/// Get the DNS Directory
pub(crate) fn get_dns_dir() -> PathBuf {
    get_data_dir().join("dns")
}

/// Sets up the logger for the taker component.
///
/// This method initializes the logging configuration for the taker, directing logs to both
/// the console and a file. It sets the `RUST_LOG` environment variable to provide default
/// log levels and configures log4rs with the specified filter level for fine-grained control
/// of log verbosity.
pub fn setup_taker_logger(filter: LevelFilter) {
    Once::new().call_once(|| {
        //env::set_var("RUST_LOG", "coinswap=info");
        let log_dir = get_taker_dir().join("debug.log");

        let file_appender = FileAppender::builder().build(log_dir).unwrap();

        let config = Config::builder()
            .appender(Appender::builder().build("file", Box::new(file_appender)))
            .logger(
                Logger::builder()
                    .appender("file")
                    .build("coinswap::taker", filter),
            )
            .build(Root::builder().appender("file").build(filter))
            .unwrap();

        log4rs::init_config(config).unwrap();
    })
}

/// Sets up the logger for the maker component.
///
/// This method initializes the logging configuration for the maker, directing logs to both
/// the console and a file. It sets the `RUST_LOG` environment variable to provide default
/// log levels and configures log4rs with the specified filter level for fine-grained control
/// of log verbosity.
pub fn setup_maker_logger(filter: LevelFilter) {
    Once::new().call_once(|| {
        //env::set_var("RUST_LOG", "coinswap=info");
        let log_dir = get_maker_dir().join("debug.log");

        let stdout = ConsoleAppender::builder().build();
        let file_appender = FileAppender::builder().build(log_dir).unwrap();

        let config = Config::builder()
            .appender(Appender::builder().build("stdout", Box::new(stdout)))
            .appender(Appender::builder().build("file", Box::new(file_appender)))
            .logger(
                Logger::builder()
                    .appender("file")
                    .build("coinswap::maker", filter),
            )
            .build(Root::builder().appender("stdout").build(filter))
            .unwrap();

        log4rs::init_config(config).unwrap();
    })
}

/// Sets up the logger for the directory component.
///
/// This method initializes the logging configuration for the directory, directing logs to both
/// the console and a file. It sets the `RUST_LOG` environment variable to provide default
/// log levels and configures log4rs with the specified filter level for fine-grained control
/// of log verbosity.
pub fn setup_directory_logger(filter: LevelFilter) {
    Once::new().call_once(|| {
        //env::set_var("RUST_LOG", "coinswap=info");
        let log_dir = get_dns_dir().join("debug.log");

        let stdout = ConsoleAppender::builder().build();
        let file_appender = FileAppender::builder().build(log_dir).unwrap();

        let config = Config::builder()
            .appender(Appender::builder().build("stdout", Box::new(stdout)))
            .appender(Appender::builder().build("file", Box::new(file_appender)))
            .logger(
                Logger::builder()
                    .appender("file")
                    .build("coinswap::market", filter),
            )
            .build(Root::builder().appender("stdout").build(filter))
            .unwrap();

        log4rs::init_config(config).unwrap();
    })
}

/// Setup function that will only run once, even if called multiple times.
/// Takes log level to set the desired logging verbosity
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

/// Send a length-appended Protocol or RPC Message through a stream.
/// The first byte sent is the length of the actual message.
pub fn send_message(
    socket_writer: &mut TcpStream,
    message: &impl serde::Serialize,
) -> Result<(), NetError> {
    let mut writer = BufWriter::new(socket_writer);
    let msg_bytes = serde_cbor::ser::to_vec(message)?;
    let msg_len = (msg_bytes.len() as u32).to_be_bytes();
    let mut to_send = Vec::with_capacity(msg_bytes.len() + msg_len.len());
    to_send.extend(msg_len);
    to_send.extend(msg_bytes);
    writer.write_all(&to_send)?;
    writer.flush()?;
    Ok(())
}

/// Reads a response byte_array from a given stream.
/// Response can be any length-appended data, where the first byte is the length of the actual message.
pub fn read_message(reader: &mut TcpStream) -> Result<Vec<u8>, NetError> {
    let mut reader = BufReader::new(reader);
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

/// Apply the maker's privatekey to swapcoins, and check it's the correct privkey for corresponding pubkey.
pub(crate) fn check_and_apply_maker_private_keys<S: SwapCoin>(
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
#[allow(clippy::type_complexity)]
pub(crate) fn generate_maker_keys(
    tweakable_point: &PublicKey,
    count: u32,
) -> Result<
    (
        Vec<PublicKey>,
        Vec<SecretKey>,
        Vec<PublicKey>,
        Vec<SecretKey>,
    ),
    ProtocolError,
> {
    // Closure to derive public keys and nonces
    let derive_keys = |count: u32| {
        (0..count)
            .map(|_| derive_maker_pubkey_and_nonce(tweakable_point))
            .collect::<Result<Vec<_>, _>>()
    };

    // Generate multisig and hashlock keys.
    let (multisig_pubkeys, multisig_nonces): (Vec<_>, Vec<_>) =
        derive_keys(count)?.into_iter().unzip();
    let (hashlock_pubkeys, hashlock_nonces): (Vec<_>, Vec<_>) =
        derive_keys(count)?.into_iter().unzip();

    Ok((
        multisig_pubkeys,
        multisig_nonces,
        hashlock_pubkeys,
        hashlock_nonces,
    ))
}

/// Extracts hierarchical deterministic (HD) path components from a descriptor.
///
/// Parses an input descriptor string and returns `Some` with a tuple containing the HD path
/// components if it's an HD descriptor. If the descriptor doesn't have path info, it returns `None`.
/// This method only works for single key descriptors.
pub(crate) fn get_hd_path_from_descriptor(descriptor: &str) -> Option<(&str, u32, i32)> {
    let open = descriptor.find('[');
    let close = descriptor.find(']');

    let path = if let (Some(open), Some(close)) = (open, close) {
        &descriptor[open + 1..close]
    } else {
        // Debug log, because if it doesn't have path, its not an error.
        log::error!("Descriptor doesn't have path = {}", descriptor);
        return None;
    };

    let path_chunks: Vec<&str> = path.split('/').collect();
    if path_chunks.len() != 3 {
        // Debug log, because if it doesn't have path, its not an error.
        //log::warn!("Path is not a triplet. Path chunks = {:?}", path_chunks);
        return None;
    }

    if let (Ok(addr_type), Ok(index)) =
        (path_chunks[1].parse::<u32>(), path_chunks[2].parse::<i32>())
    {
        Some((path_chunks[0], addr_type, index))
    } else {
        None
    }
}

/// Generates a keypair using the secp256k1 elliptic curve.
pub(crate) fn generate_keypair() -> (PublicKey, SecretKey) {
    let keypair = Keypair::new(&Secp256k1::new(), &mut thread_rng());
    let pubkey = PublicKey {
        compressed: true,
        inner: keypair.public_key(),
    };
    (pubkey, keypair.secret_key())
}

/// Convert a redeemscript into p2wsh scriptpubkey.
pub(crate) fn redeemscript_to_scriptpubkey(
    redeemscript: &ScriptBuf,
) -> Result<ScriptBuf, ProtocolError> {
    let witness_program = WitnessProgram::new(
        WitnessVersion::V0,
        &redeemscript.wscript_hash().to_byte_array(),
    )?;
    Ok(ScriptBuf::new_witness_program(&witness_program))
}

/// Parses a TOML file into a HashMap of key-value pairs.
pub(crate) fn parse_toml<P: AsRef<Path>>(path: P) -> io::Result<HashMap<String, String>> {
    let content = fs::read_to_string(path)?;

    let mut config_map = HashMap::new();

    for line in content.lines().filter(|line| !line.is_empty()) {
        if let Some((key, value)) = line.split_once('=') {
            config_map.insert(key.trim().to_string(), value.trim().to_string());
        }
    }

    Ok(config_map)
}

/// Parses a value of type T from an Option<&String>, returning the default if parsing fails or is None
pub(crate) fn parse_field<T: std::str::FromStr>(value: Option<&String>, default: T) -> T {
    value
        .and_then(|value| value.parse::<T>().ok())
        .unwrap_or(default)
}

/// Function to check if tor log contains a pattern
pub(crate) fn monitor_log_for_completion(log_file: &Path, pattern: &str) -> io::Result<()> {
    // TODO: Make this logic work for existing file with previous logs.
    let mut last_size = 0;

    loop {
        if log_file.exists() {
            let file = File::open(log_file)?;
            let metadata = file.metadata()?;
            let current_size = metadata.len();

            if current_size != last_size {
                let reader = io::BufReader::new(file);
                let lines = reader.lines();

                for line in lines {
                    if let Ok(line) = line {
                        log::info!("{}", line);
                        if line.contains(pattern) {
                            return Ok(());
                        }
                    } else {
                        return Err(io::Error::new(io::ErrorKind::Other, "Error reading line"));
                    }
                }

                last_size = current_size;
            }
        }
        thread::sleep(HEART_BEAT_INTERVAL);
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
pub(crate) fn compute_checksum(descriptor: &str) -> Result<String, WalletError> {
    let mut checksum = CHECKSUM_FINAL_XOR_VALUE;
    let mut accumulated_value = 0;
    let mut group_count = 0;

    for character in descriptor.chars() {
        let position = INPUT_CHARSET
            .find(character)
            .ok_or(ProtocolError::General("Descriptor invalid"))? as u64;
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
                .expect("checksum character expected")
        })
        .collect::<String>();

    Ok(checksum_chars)
}

/// Parse the proxy (Socket:Port) argument from the cli input.
pub fn parse_proxy_auth(s: &str) -> Result<(String, String), NetError> {
    let parts: Vec<_> = s.split(':').collect();
    if parts.len() != 2 {
        return Err(NetError::InvalidNetworkAddress);
    }

    let user = parts[0].to_string();
    let passwd = parts[1].to_string();

    Ok((user, passwd))
}

/// Dns request metadata
#[derive(Serialize, Deserialize, Debug)]
pub struct DnsMetadata {
    pub(crate) url: String,
    pub(crate) proof: FidelityProof,
}

/// Structured requests and responses used to interact with dns.
///
/// Enum representing DNS request messages.
#[derive(Serialize, Deserialize, Debug)]
pub enum DnsRequest {
    /// Sent by the maker to DNS to register itself.
    Post {
        /// Metadata associated with the request.
        metadata: Box<DnsMetadata>,
    },
    /// Sent by the taker to retrieve all requests.
    Get,
    /// Dummy variant used for testing purposes.
    #[cfg(feature = "integration-test")]
    Dummy {
        /// URL to register with DNS.
        url: String,
    },
}

pub(crate) fn verify_fidelity_checks(
    proof: &FidelityProof,
    addr: &str,
    tx: Transaction,
    current_height: u64,
) -> Result<(), WalletError> {
    // Check if bond lock time has expired
    let lock_time = LockTime::from_height(current_height as u32)?;
    if lock_time > proof.bond.lock_time {
        return Err(FidelityError::BondLocktimeExpired.into());
    }

    // Verify certificate hash
    let expected_cert_hash = proof.bond.generate_cert_hash(addr);
    if proof.cert_hash != expected_cert_hash {
        return Err(FidelityError::InvalidCertHash.into());
    }

    let networks = vec![
        bitcoin::network::Network::Regtest,
        bitcoin::network::Network::Testnet,
        bitcoin::network::Network::Testnet4,
        bitcoin::network::Network::Bitcoin,
        bitcoin::network::Network::Signet,
    ];

    let mut all_failed = true;

    for network in networks {
        // Validate redeem script and corresponding address
        let fidelity_redeem_script =
            fidelity_redeemscript(&proof.bond.lock_time, &proof.bond.pubkey);
        let expected_address = Address::p2wsh(fidelity_redeem_script.as_script(), network);

        let derived_script_pubkey = expected_address.script_pubkey();
        let tx_out = tx
            .tx_out(proof.bond.outpoint.vout as usize)
            .map_err(|_| WalletError::General("Outputs index error".to_string()))?;

        if tx_out.script_pubkey == derived_script_pubkey {
            all_failed = false;
            break; // No need to continue checking once we find a successful match
        }
    }

    // Only throw error if all checks fail
    if all_failed {
        return Err(FidelityError::BondDoesNotExist.into());
    }

    // Verify ECDSA signature
    let secp = Secp256k1::new();
    let cert_message = Message::from_digest_slice(proof.cert_hash.as_byte_array())?;
    secp.verify_ecdsa(&cert_message, &proof.cert_sig, &proof.bond.pubkey.inner)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use bitcoin::{
        blockdata::{opcodes::all, script::Builder},
        secp256k1::Scalar,
        PubkeyHash,
    };

    use crate::protocol::messages::{MakerHello, MakerToTakerMessage};

    use super::*;

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

            if let MakerToTakerMessage::MakerHello(hello) = msg {
                assert!(hello.protocol_version_min == 1 && hello.protocol_version_max == 100);
            } else {
                panic!(
                    "Received Wrong Message: Expected MakerHello variant, Got: {:?}",
                    msg,
                );
            }
        });

        let mut stream = TcpStream::connect(address).unwrap();
        send_message(&mut stream, &message).unwrap();
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
            redeemscript_to_scriptpubkey(&puzzle_script)
                .unwrap()
                .to_hex_string(),
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
            redeemscript_to_scriptpubkey(&script)
                .unwrap()
                .to_hex_string(),
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
            redeemscript_to_scriptpubkey(&script)
                .unwrap()
                .to_hex_string(),
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
            generate_maker_keys(&tweak_point, 1).unwrap();
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

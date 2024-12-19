//! A simple directory-server
//!
//! Handles market-related logic where Makers post their offers. Also provides functions to synchronize
//! maker addresses from directory servers, post maker addresses to directory servers,

use bip39::Mnemonic;
use bitcoin::{
    absolute::LockTime, hashes::Hash, key::Secp256k1, secp256k1::Message,
    transaction::OutputsIndexError, Address,
};
use bitcoind::bitcoincore_rpc::RpcApi;
use serde::{Deserialize, Serialize};

use crate::{
    market::rpc::start_rpc_server_thread,
    protocol::messages::FidelityProof,
    utill::{
        get_dns_dir, get_tor_addrs, monitor_log_for_completion, parse_field, parse_toml,
        seed_phrase_to_unique_id, ConnectionType,
    },
    wallet::{fidelity_redeemscript, RPCConfig, Wallet, WalletError},
};

use std::{
    collections::HashSet,
    fs::{self, File},
    io::{BufRead, BufReader, BufWriter, Read, Write},
    net::{Ipv4Addr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering::Relaxed},
        Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard,
    },
    thread::{self, sleep},
    time::Duration,
};

use crate::error::NetError;

#[derive(Serialize, Deserialize, Debug)]
struct OfferMetadata {
    url: String,
    proof: FidelityProof,
}

// Structured requests and responses using serde.
#[derive(Serialize, Deserialize, Debug)]
enum Request {
    Post { metadata: Box<OfferMetadata> },
    Get { makers: u32 },
    Dummy { url: String },
}

/// Represents errors that can occur during directory server operations.
#[derive(Debug)]
pub enum DirectoryServerError {
    IO(std::io::Error),
    Net(NetError),
    MutexPossion,
    Wallet(WalletError),
    Cbor(serde_cbor::Error),
    Rpc(bitcoind::bitcoincore_rpc::Error),
    OutputsIndexError(OutputsIndexError),
    Locktime(bitcoin::blockdata::locktime::absolute::ConversionError),
    Secp(bitcoin::secp256k1::Error),
}

impl From<WalletError> for DirectoryServerError {
    fn from(value: WalletError) -> Self {
        Self::Wallet(value)
    }
}

impl From<OutputsIndexError> for DirectoryServerError {
    fn from(value: OutputsIndexError) -> Self {
        Self::OutputsIndexError(value)
    }
}

impl From<serde_cbor::Error> for DirectoryServerError {
    fn from(value: serde_cbor::Error) -> Self {
        Self::Cbor(value)
    }
}

impl From<bitcoind::bitcoincore_rpc::Error> for DirectoryServerError {
    fn from(value: bitcoind::bitcoincore_rpc::Error) -> Self {
        Self::Rpc(value)
    }
}

impl From<bitcoin::blockdata::locktime::absolute::ConversionError> for DirectoryServerError {
    fn from(value: bitcoin::blockdata::locktime::absolute::ConversionError) -> Self {
        Self::Locktime(value)
    }
}

impl From<bitcoin::secp256k1::Error> for DirectoryServerError {
    fn from(value: bitcoin::secp256k1::Error) -> Self {
        Self::Secp(value)
    }
}

impl From<std::io::Error> for DirectoryServerError {
    fn from(value: std::io::Error) -> Self {
        Self::IO(value)
    }
}

impl From<NetError> for DirectoryServerError {
    fn from(value: NetError) -> Self {
        Self::Net(value)
    }
}

impl<'a, T> From<PoisonError<RwLockReadGuard<'a, T>>> for DirectoryServerError {
    fn from(_: PoisonError<RwLockReadGuard<'a, T>>) -> Self {
        Self::MutexPossion
    }
}

impl<'a, T> From<PoisonError<RwLockWriteGuard<'a, T>>> for DirectoryServerError {
    fn from(_: PoisonError<RwLockWriteGuard<'a, T>>) -> Self {
        Self::MutexPossion
    }
}

/// Directory Configuration,
#[derive(Debug)]
pub struct DirectoryServer {
    pub rpc_port: u16,
    pub port: u16,
    pub socks_port: u16,
    pub connection_type: ConnectionType,
    pub data_dir: PathBuf,
    pub shutdown: AtomicBool,
    pub addresses: Arc<RwLock<HashSet<String>>>,
}

impl Default for DirectoryServer {
    fn default() -> Self {
        Self {
            rpc_port: 4321,
            port: 8080,
            socks_port: 19060,
            connection_type: ConnectionType::TOR,
            data_dir: get_dns_dir(),
            shutdown: AtomicBool::new(false),
            addresses: Arc::new(RwLock::new(HashSet::new())),
        }
    }
}

impl DirectoryServer {
    /// Constructs a [DirectoryConfig] from a specified data directory. Or create default configs and load them.
    ///
    /// The directory.toml file should exist at the provided data-dir location.
    /// Or else, a new default-config will be loaded and created at given data-dir location.
    /// If no data-dir is provided, a default config will be created at default data-dir location.
    ///
    /// For reference of default config checkout `./directory.toml` in repo folder.
    ///
    /// Default data-dir for linux: `~/.coinswap/dns`
    /// Default config locations: `~/.coinswap/dns/config.toml`.
    pub fn new(
        data_dir: Option<PathBuf>,
        connection_type: Option<ConnectionType>,
    ) -> Result<Self, DirectoryServerError> {
        let data_dir = data_dir.unwrap_or(get_dns_dir());
        let config_path = data_dir.join("config.toml");

        // This will create parent directories if they don't exist
        if !config_path.exists() || fs::metadata(&config_path)?.len() == 0 {
            log::warn!(
                "Directory config file not found, creating default config file at path: {}",
                config_path.display()
            );
            write_default_directory_config(&config_path)?;
        }

        let mut config_map = parse_toml(&config_path)?;

        log::info!(
            "Successfully loaded config file from : {}",
            config_path.display()
        );

        // Update the connection type in config if given.
        if let Some(conn_type) = connection_type {
            // update the config map
            let value = config_map.get_mut("connection_type").expect("must exist");
            let conn_type_string = format!("{:?}", conn_type);
            *value = conn_type_string;

            // Update the file on disk
            let mut file = File::create(config_path)?;
            let mut content = String::new();

            for (i, (key, value)) in config_map.iter().enumerate() {
                // Format each line, adding a newline for all except the last one
                content.push_str(&format!("{} = {}", key, value));
                if i < config_map.len() - 1 {
                    content.push('\n');
                }
            }

            file.write_all(content.as_bytes())?;
        }

        let addresses = Arc::new(RwLock::new(HashSet::new()));
        let address_file = data_dir.join("addresses.dat");
        if let Ok(file) = File::open(&address_file) {
            let reader = BufReader::new(file);
            for address in reader.lines().map_while(Result::ok) {
                addresses.write()?.insert(address);
            }
        }
        let default_dns = Self::default();

        Ok(DirectoryServer {
            rpc_port: parse_field(config_map.get("rpc_port"), default_dns.rpc_port),
            port: parse_field(config_map.get("port"), default_dns.port),
            socks_port: parse_field(config_map.get("socks_port"), default_dns.socks_port),
            data_dir,
            shutdown: AtomicBool::new(false),
            connection_type: parse_field(
                config_map.get("connection_type"),
                default_dns.connection_type,
            ),
            addresses,
        })
    }
}

fn write_default_directory_config(config_path: &PathBuf) -> Result<(), DirectoryServerError> {
    let config_string = String::from(
        "\
            port = 8080\n\
            socks_port = 19060\n\
            connection_type = tor\n\
            rpc_port = 4321\n\
            ",
    );
    std::fs::create_dir_all(config_path.parent().expect("Path should NOT be root!"))?;
    let mut file = File::create(config_path)?;
    file.write_all(config_string.as_bytes())?;
    file.flush()?;
    Ok(())
}

pub fn start_address_writer_thread(
    directory: Arc<DirectoryServer>,
) -> Result<(), DirectoryServerError> {
    let address_file = directory.data_dir.join("addresses.dat");

    let interval = if cfg!(feature = "integration-test") {
        3 // 3 seconds for tests
    } else {
        600 // 10 minutes for production
    };
    loop {
        sleep(Duration::from_secs(interval));

        if let Err(e) = write_addresses_to_file(&directory, &address_file) {
            log::error!("Error writing addresses: {:?}", e);
        }
    }
}

pub fn write_addresses_to_file(
    directory: &Arc<DirectoryServer>,
    address_file: &Path,
) -> Result<(), DirectoryServerError> {
    let file_content = directory
        .addresses
        .read()?
        .iter()
        .map(|addr| format!("{}\n", addr))
        .collect::<Vec<String>>()
        .join("");

    let mut file = File::create(address_file)?;
    file.write_all(file_content.as_bytes())?;
    file.flush()?;
    Ok(())
}
pub fn start_directory_server(
    directory: Arc<DirectoryServer>,
    rpc_config: Option<RPCConfig>,
) -> Result<(), DirectoryServerError> {
    let mut tor_handle = None;

    let mut rpc_config = rpc_config.unwrap_or_default();

    let wallet_file_name = None;
    let wallets_dir = PathBuf::from("/tmp/.coinswap/directory");
    let mut wallet = if let Some(file_name) = wallet_file_name {
        let wallet_path = wallets_dir.join(&file_name);
        rpc_config.wallet_name = file_name;
        if wallet_path.exists() {
            // Try loading wallet
            let wallet = Wallet::load(&rpc_config, &wallet_path)?;
            log::info!("Wallet file at {:?} successfully loaded.", wallet_path);
            wallet
        } else {
            // Create wallet with the given name.
            let mnemonic = Mnemonic::generate(12).map_err(WalletError::BIP39)?;
            let seedphrase = mnemonic.to_string();

            let wallet = Wallet::init(&wallet_path, &rpc_config, seedphrase, "".to_string())?;
            log::info!("New Wallet created at : {:?}", wallet_path);
            wallet
        }
    } else {
        // Create default wallet
        let mnemonic = Mnemonic::generate(12).map_err(WalletError::BIP39)?;
        let seedphrase = mnemonic.to_string();

        // File names are unique for default wallets
        let unique_id = seed_phrase_to_unique_id(&seedphrase);
        let file_name = unique_id + "-directory";
        let wallet_path = wallets_dir.join(&file_name);
        rpc_config.wallet_name = file_name;
        let wallet = Wallet::init(&wallet_path, &rpc_config, seedphrase, "".to_string())?;
        log::info!("New Wallet created at : {:?}", wallet_path);
        wallet
    };

    log::info!("Initializing directory wallet sync");
    wallet.sync()?;
    log::info!("Completed Directory wallet sync");

    match directory.connection_type {
        ConnectionType::CLEARNET => {}
        ConnectionType::TOR => {
            if cfg!(feature = "tor") {
                let tor_log_dir = "/tmp/tor-rust-directory/log".to_string();
                if Path::new(tor_log_dir.as_str()).exists() {
                    match std::fs::remove_file(Path::new(tor_log_dir.clone().as_str())) {
                        Ok(_) => log::info!("Previous directory log file deleted successfully"),
                        Err(_) => log::error!("Error deleting directory log file"),
                    }
                }

                let socks_port = directory.socks_port;
                let tor_port = directory.port;
                tor_handle = Some(crate::tor::spawn_tor(
                    socks_port,
                    tor_port,
                    "/tmp/tor-rust-directory".to_string(),
                ));

                sleep(Duration::from_secs(10));

                if let Err(e) = monitor_log_for_completion(&PathBuf::from(tor_log_dir), "100%") {
                    log::error!("Error monitoring Directory log file: {}", e);
                }

                log::info!("Directory tor is instantiated");

                let onion_addr = get_tor_addrs(&PathBuf::from("/tmp/tor-rust-directory"))?;

                log::info!(
                    "Directory Server is listening at {}:{}",
                    onion_addr,
                    tor_port
                );
            }
        }
    }

    let directory_clone = directory.clone();

    let rpc_thread = thread::spawn(move || {
        log::info!("Spawning RPC Server Thread");
        start_rpc_server_thread(directory_clone)
    });

    let address_file = directory.data_dir.join("addresses.dat");
    let directory_clone = directory.clone();
    let address_writer_thread = thread::spawn(move || {
        log::info!("Spawning Address Writer Thread");
        start_address_writer_thread(directory_clone)
    });

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, directory.port))?;

    while !directory.shutdown.load(Relaxed) {
        match listener.accept() {
            Ok((mut stream, addrs)) => {
                log::debug!("Incoming connection from : {}", addrs);
                stream.set_read_timeout(Some(Duration::from_secs(60)))?;
                stream.set_write_timeout(Some(Duration::from_secs(60)))?;
                handle_client(&mut stream, &directory.clone(), &wallet)?;
            }

            // If no connection received, check for shutdown or save addresses to disk
            Err(e) => {
                log::error!("Error accepting incoming connection: {:?}", e);
            }
        }

        sleep(Duration::from_secs(3));
    }

    log::info!("Shutdown signal received. Stopping directory server.");

    // Its okay to suppress the error here as we are shuting down anyway.
    if let Err(e) = rpc_thread.join() {
        log::error!("Error closing RPC Thread: {:?}", e);
    }
    if let Err(e) = address_writer_thread.join() {
        log::error!("Error closing Address Writer Thread : {:?}", e);
    }

    if let Some(handle) = tor_handle {
        crate::tor::kill_tor_handles(handle);
        log::info!("Directory server and Tor instance terminated successfully");
    }

    write_addresses_to_file(&directory, &address_file)?;

    Ok(())
}

// The stream should have read and write timeout set.
fn handle_client(
    stream: &mut TcpStream,
    directory: &Arc<DirectoryServer>,
    wallet: &Wallet,
) -> Result<(), DirectoryServerError> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = BufWriter::new(stream);
    let mut length_bytes = [0u8; 8];
    reader.read_exact(&mut length_bytes)?;
    let length = u64::from_be_bytes(length_bytes);

    let mut buf = vec![0; length as usize];
    reader.read_exact(&mut buf)?;
    let value: Request = serde_cbor::de::from_reader(&buf[..])?;
    match value {
        Request::Post { metadata } => {
            log::info!("Received new maker address: {}", &metadata.url);

            let fidelity_redeem_script =
                fidelity_redeemscript(&metadata.proof.bond.lock_time, &metadata.proof.bond.pubkey);
            let address = Address::p2wsh(
                fidelity_redeem_script.as_script(),
                bitcoin::network::Network::Regtest,
            );
            let script_pubkey = &address.script_pubkey();

            let txid = metadata.proof.bond.outpoint.txid;
            let transaction = wallet.rpc.get_raw_transaction(&txid, None)?;
            let tx_out = transaction.tx_out(0)?;
            let redeem_script_pubkey = &tx_out.script_pubkey;

            let cert_hash = metadata.proof.cert_hash;
            let calculated_cert_hash = metadata.proof.bond.generate_cert_hash(&metadata.url);

            let secp = Secp256k1::new();
            let cert_message = match Message::from_digest_slice(cert_hash.as_byte_array()) {
                Ok(msg) => msg,
                Err(e) => {
                    log::error!("Failed to create ECDSA message: {}", e);
                    return Err(e.into());
                }
            };

            let ecdsa_verified = secp
                .verify_ecdsa(
                    &cert_message,
                    &metadata.proof.cert_sig,
                    &metadata.proof.bond.pubkey.inner,
                )
                .is_ok();

            let current_height = wallet.rpc.get_block_count()?;
            let lock_time_valid =
                LockTime::from_height(current_height as u32)? < metadata.proof.bond.lock_time;

            if script_pubkey == redeem_script_pubkey
                && ecdsa_verified
                && lock_time_valid
                && cert_hash == calculated_cert_hash
            {
                log::info!("Maker verified successfully.");
                let mut addresses = directory
                    .addresses
                    .write()
                    .expect("Failed to acquire write lock");
                addresses.insert(metadata.url);
            } else {
                log::warn!("Potentially suspicious maker detected.");
            }
        }
        Request::Get { makers } => {
            log::info!("Taker pinged the directory server");
            log::info!("Number of makers requested: {:?}", makers);
            let addresses = directory.addresses.read()?;
            let response = if addresses.len() < makers as usize {
                String::new()
            } else {
                addresses
                    .iter()
                    .take(makers as usize)
                    .fold(String::new(), |acc, addr| acc + addr + "\n")
            };
            writer.write_all(response.as_bytes())?;
            writer.flush()?;
        }
        Request::Dummy { url } => {
            log::info!("Got new maker address: {}", &url);
            directory.addresses.write()?.insert(url);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoind::tempfile::TempDir;

    fn create_temp_config(contents: &str, temp_dir: &TempDir) -> PathBuf {
        let config_path = temp_dir.path().join("config.toml");
        std::fs::write(&config_path, contents).unwrap();
        config_path
    }

    #[test]
    fn test_valid_config() {
        let temp_dir = TempDir::new().unwrap();
        let contents = r#"
            [directory_config]
            port = 8080
            socks_port = 19060
        "#;
        create_temp_config(contents, &temp_dir);
        let dns = DirectoryServer::new(Some(temp_dir.path().to_path_buf()), None).unwrap();
        let default_dns = DirectoryServer::default();

        assert_eq!(dns.port, default_dns.port);
        assert_eq!(dns.socks_port, default_dns.socks_port);

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_missing_fields() {
        let temp_dir = TempDir::new().unwrap();
        let contents = r#"
            [directory_config]
            port = 8080
        "#;
        create_temp_config(contents, &temp_dir);
        let dns = DirectoryServer::new(Some(temp_dir.path().to_path_buf()), None).unwrap();

        assert_eq!(dns.port, 8080);
        assert_eq!(dns.socks_port, DirectoryServer::default().socks_port);

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_incorrect_data_type() {
        let temp_dir = TempDir::new().unwrap();
        let contents = r#"
            [directory_config]
            port = "not_a_number"
        "#;
        create_temp_config(contents, &temp_dir);
        let dns = DirectoryServer::new(Some(temp_dir.path().to_path_buf()), None).unwrap();
        let default_dns = DirectoryServer::default();

        assert_eq!(dns.port, default_dns.port);
        assert_eq!(dns.socks_port, default_dns.socks_port);

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_missing_file() {
        let temp_dir = TempDir::new().unwrap();
        let dns = DirectoryServer::new(Some(temp_dir.path().to_path_buf()), None).unwrap();
        let default_dns = DirectoryServer::default();

        assert_eq!(dns.port, default_dns.port);
        assert_eq!(dns.socks_port, default_dns.socks_port);

        temp_dir.close().unwrap();
    }
}

//! A simple directory-server
//!
//! Handles market-related logic where Makers post their offers. Also provides functions to synchronize
//! maker addresses from directory servers, post maker addresses to directory servers.

use bitcoin::{transaction::ParseOutPointError, OutPoint};
use bitcoind::bitcoincore_rpc::{self, Client, RpcApi};

use crate::{
    market::rpc::start_rpc_server_thread,
    protocol::messages::{DnsRequest, DnsResponse},
    utill::{
        check_tor_status, get_dns_dir, get_tor_hostname, parse_field, parse_toml, read_message,
        send_message, verify_fidelity_checks, ConnectionType, TorError, HEART_BEAT_INTERVAL,
    },
    wallet::{RPCConfig, WalletError},
};

use std::{
    collections::HashMap,
    convert::TryFrom,
    fs::{self, File},
    io::Write,
    net::{Ipv4Addr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering::Relaxed},
        Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard,
    },
    thread::{self, sleep},
    time::{Duration, Instant},
};

use crate::error::NetError;

/// Represents errors that may occur during directory server operations.
#[derive(Debug)]
pub enum DirectoryServerError {
    /// Error originating from standard I/O operations.
    ///
    /// This variant wraps a [`std::io::Error`] to provide details about I/O failures encountered during directory server operations.
    IO(std::io::Error),

    /// Error related to network operations.
    ///
    /// This variant wraps a [`NetError`] to represent various network-related issues.
    Net(NetError),

    /// Error indicating a mutex was poisoned.
    ///
    /// This occurs when a thread panics while holding a mutex, rendering it unusable.
    MutexPossion,

    /// Error related to wallet operations.
    ///
    /// This variant wraps a [`WalletError`] to capture issues arising during wallet-related operations.
    Wallet(WalletError),
    /// Error indicating the address.dat file is corrupted.
    ///
    /// This can occur in case of incomplete shutdown or other ways a file can corrupt.
    AddressFileCorrupted(String),
    /// Error related to tor
    TorError(TorError),
}

impl From<TorError> for DirectoryServerError {
    fn from(value: TorError) -> Self {
        Self::TorError(value)
    }
}

impl From<WalletError> for DirectoryServerError {
    fn from(value: WalletError) -> Self {
        Self::Wallet(value)
    }
}

impl From<serde_cbor::Error> for DirectoryServerError {
    fn from(value: serde_cbor::Error) -> Self {
        Self::Wallet(WalletError::Cbor(value))
    }
}

impl From<bitcoind::bitcoincore_rpc::Error> for DirectoryServerError {
    fn from(value: bitcoind::bitcoincore_rpc::Error) -> Self {
        Self::Wallet(WalletError::Rpc(value))
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

impl From<ParseOutPointError> for DirectoryServerError {
    fn from(value: ParseOutPointError) -> Self {
        Self::AddressFileCorrupted(value.to_string())
    }
}

/// Directory Configuration,
#[derive(Debug)]
pub struct DirectoryServer {
    /// RPC listening port
    pub rpc_port: u16,
    /// Network listening port
    pub network_port: u16,
    /// Control port
    pub control_port: u16,
    /// Socks port
    pub socks_port: u16,
    /// Authentication password
    pub tor_auth_password: String,
    /// Connection type
    pub connection_type: ConnectionType,
    /// Directory server data directory
    pub data_dir: PathBuf,
    /// Shutdown flag to stop the directory server
    pub shutdown: AtomicBool,
    /// A store of all the received maker addresses indexed by fidelity bond outpoints.
    pub addresses: Arc<RwLock<HashMap<OutPoint, (String, Instant)>>>,
}

impl Default for DirectoryServer {
    fn default() -> Self {
        Self {
            rpc_port: 4321,
            network_port: 8080,
            socks_port: 9050,
            control_port: 9051,
            tor_auth_password: "".to_string(),
            connection_type: if cfg!(feature = "integration-test") {
                ConnectionType::CLEARNET
            } else {
                ConnectionType::TOR
            },
            data_dir: get_dns_dir(),
            shutdown: AtomicBool::new(false),
            addresses: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl DirectoryServer {
    /// Constructs a [DirectoryServer] from a specified data directory. Or creates default configs and load them.
    ///
    /// The directory.toml file should exist at the provided data-dir location.
    /// Or else, a new default-config will be loaded and created at the given data-dir location.
    /// If no data-dir is provided, a default config will be created at the default data-dir location.
    ///
    /// For reference of default config checkout `./directory.toml` in repo folder.
    ///
    /// Default data-dir for linux: `~/.coinswap/dns`
    /// Default config locations: `~/.coinswap/dns/config.toml`.
    #[allow(unused_mut)]
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
            // Update the config map
            let value = config_map.get_mut("connection_type").expect("must exist");
            let conn_type_string = format!("{conn_type:?}");
            *value = conn_type_string;

            // Update the file on disk
            let mut config_file = File::create(config_path)?;
            let mut content = String::new();

            for (i, (key, value)) in config_map.iter().enumerate() {
                // Format each line, adding a newline for all except the last one
                content.push_str(&format!("{key} = {value}"));
                if i < config_map.len() - 1 {
                    content.push('\n');
                }
            }

            config_file.write_all(content.as_bytes())?;
        }

        let addresses = Arc::new(RwLock::new(HashMap::new()));
        let default_dns = Self::default();

        let mut config = DirectoryServer {
            rpc_port: parse_field(config_map.get("rpc_port"), default_dns.rpc_port),
            network_port: parse_field(config_map.get("network_port"), default_dns.network_port),
            socks_port: parse_field(config_map.get("socks_port"), default_dns.socks_port),
            control_port: parse_field(config_map.get("control_port"), default_dns.control_port),
            tor_auth_password: parse_field(
                config_map.get("tor_auth_password"),
                default_dns.tor_auth_password,
            ),
            data_dir: data_dir.clone(),
            shutdown: AtomicBool::new(false),
            connection_type: parse_field(
                config_map.get("connection_type"),
                default_dns.connection_type,
            ),
            addresses,
        };

        if matches!(connection_type, Some(ConnectionType::TOR)) {
            check_tor_status(config.control_port, &config.tor_auth_password)?;
        }
        Ok(config)
    }

    /// Updates the in-memory address map. If entry already exists, updates the value. If new entry, inserts the value.
    pub fn updated_address_map(
        &self,
        metadata: (String, OutPoint),
    ) -> Result<(), DirectoryServerError> {
        let mut write_lock = self.addresses.write()?;
        // Check if the value exists with a different key
        if let Some(existing_key) =
            write_lock
                .iter()
                .find_map(|(k, v)| if v.0 == metadata.0 { Some(*k) } else { None })
        {
            // Update the fidelity for the existing address
            if existing_key != metadata.1 {
                log::info!(
                    "Fidelity update detected for address: {} | Old fidelity {} | New fidelity {}",
                    metadata.0,
                    existing_key,
                    metadata.1
                );
                write_lock.remove(&existing_key);
                write_lock.insert(metadata.1, (metadata.0, Instant::now()));
            } else {
                log::info!(
                    "Maker data already exist for {} | restarted counter",
                    metadata.0
                );
                write_lock
                    .entry(metadata.1)
                    .and_modify(|(_, instant)| *instant = Instant::now());
            }
        } else if write_lock.contains_key(&metadata.1) {
            // Update the address for the existing fidelity
            if write_lock[&metadata.1].0 != metadata.0 {
                let old_addr = write_lock
                    .insert(metadata.1, (metadata.0.clone(), Instant::now()))
                    .expect("value expected");
                log::info!(
                    "Address updated for fidelity: {} | old address {:?} | new address {}",
                    metadata.1,
                    old_addr,
                    metadata.0
                );
            } else {
                log::info!(
                    "Maker data already exist for {} | restarted counter",
                    metadata.0
                );
                write_lock
                    .entry(metadata.1)
                    .and_modify(|(_, instant)| *instant = Instant::now());
            }
        } else {
            // Add a new entry if both fidelity and address are new
            write_lock.insert(metadata.1, (metadata.0.clone(), Instant::now()));
            log::info!(
                "Added new maker info: Fidelity {} | Address {}",
                metadata.1,
                metadata.0
            );
        }
        Ok(())
    }
}

fn write_default_directory_config(config_path: &Path) -> Result<(), DirectoryServerError> {
    let config_string = String::from(
        "\
            network_port = 8080\n\
            socks_port = 9050\n\
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

pub(crate) fn start_address_writer_thread(
    directory: Arc<DirectoryServer>,
) -> Result<(), DirectoryServerError> {
    let interval = 60 * 15;
    loop {
        sleep(Duration::from_secs(interval));
        let mut directory_address_book = directory.addresses.write()?;
        let ttl = Duration::from_secs(60 * 30);

        let expired_outpoints: Vec<_> = directory_address_book
            .iter()
            .filter(|(_, (_, timestamp))| timestamp.elapsed() > ttl)
            .map(|(outpoint, _)| *outpoint)
            .collect();
        for outpoint in &expired_outpoints {
            log::info!("No update for 30 mins from maker with fidelity : {outpoint}");
            directory_address_book.remove(outpoint);
            log::info!("Maker entry removed");
        }
    }
}

/// Initializes and starts the Directory Server with the provided configuration.
///
/// This function configures the Directory Server based on the specified `directory` and optional `rpc_config`.
/// It handles both Clearnet and Tor connections (if the `tor` feature is enabled) and performs the following tasks:
///
/// - Sets up the Directory Server for the appropriate connection type.
/// - Spawns threads for handling RPC requests and writing address data to disk.
/// - Monitors and manages incoming TCP connections.
/// - Handles shutdown signals gracefully, ensuring all threads are terminated and resources are cleaned up.
///
pub fn start_directory_server(
    directory: Arc<DirectoryServer>,
    rpc_config: Option<RPCConfig>,
) -> Result<(), DirectoryServerError> {
    let rpc_config = rpc_config.unwrap_or_default();

    let rpc_client = bitcoincore_rpc::Client::try_from(&rpc_config)?;

    // Stop early if bitcoin core connection is wrong
    if let Err(e) = rpc_client.get_blockchain_info() {
        log::error!("Cannot connect to bitcoin node {e:?}");
        return Err(e.into());
    } else {
        log::info!("Bitcoin core connection successful");
    }

    match directory.connection_type {
        ConnectionType::CLEARNET => {}
        ConnectionType::TOR => {
            let network_port = directory.network_port;
            log::info!("tor is ready!!");
            let hostname = get_tor_hostname(
                &directory.data_dir,
                directory.control_port,
                directory.network_port,
                &directory.tor_auth_password,
            )?;
            log::info!("DNS is listening at {hostname}:{network_port}");
        }
    }

    let directory_clone = directory.clone();

    let rpc_thread = thread::spawn(move || {
        log::info!("Spawning RPC Server Thread");
        start_rpc_server_thread(directory_clone)
    });

    let directory_clone = directory.clone();
    let address_writer_thread = thread::spawn(move || {
        log::info!("Spawning Address Writer Thread");
        start_address_writer_thread(directory_clone)
    });

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, directory.network_port))?;

    while !directory.shutdown.load(Relaxed) {
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream.set_read_timeout(Some(Duration::from_secs(60)))?;
                stream.set_write_timeout(Some(Duration::from_secs(60)))?;
                if let Err(e) = handle_client(&mut stream, &directory, &rpc_client) {
                    log::error!("Error accepting incoming connection: {e:?}");
                }
            }

            // If no connection received, check for shutdown or save addresses to disk
            Err(e) => {
                log::error!("Error accepting incoming connection: {e:?}");
            }
        }

        sleep(HEART_BEAT_INTERVAL);
    }

    log::info!("Shutdown signal received. Stopping directory server.");

    // It's okay to suppress the error here as we are shutting down anyway.
    if let Err(e) = rpc_thread.join() {
        log::error!("Error closing RPC Thread: {e:?}");
    }
    if let Err(e) = address_writer_thread.join() {
        log::error!("Error closing Address Writer Thread : {e:?}");
    }

    Ok(())
}

// The stream should have read and write timeout set.
fn handle_client(
    stream: &mut TcpStream,
    directory: &Arc<DirectoryServer>,
    rpc: &Client,
) -> Result<(), DirectoryServerError> {
    let buf = read_message(&mut stream.try_clone()?)?;
    let dns_request: DnsRequest = serde_cbor::de::from_reader(&buf[..])?;
    match dns_request {
        DnsRequest::Post { metadata } => {
            let fidelity_op = metadata.proof.bond.outpoint;
            log::info!(
                "Received POST | Maker {} | Outpoint {}",
                &metadata.url,
                fidelity_op
            );
            let transaction = rpc.get_raw_transaction(&fidelity_op.txid, None)?;
            let current_height = rpc.get_block_count()?;

            match verify_fidelity_checks(
                &metadata.proof,
                &metadata.url,
                transaction,
                current_height,
            ) {
                Ok(_) => {
                    log::info!(
                        "Fidelity verification success from {}. Adding/updating to address data.",
                        metadata.url
                    );

                    match directory
                        .updated_address_map((metadata.url.clone(), metadata.proof.bond.outpoint))
                    {
                        Ok(_) => {
                            log::info!("Maker posting request successful from {}", metadata.url);
                            send_message(stream, &DnsResponse::Ack)?;
                        }
                        Err(e) => {
                            log::warn!("Maker posting request failed from {}", metadata.url);
                            send_message(
                                stream,
                                &DnsResponse::Nack(format!("Maker posting request failed: {e:?}")),
                            )?;
                        }
                    }
                }
                Err(e) => {
                    log::error!(
                        "Potentially suspicious maker detected: {:?} | {:?}",
                        metadata.url,
                        e
                    );
                    send_message(
                        stream,
                        &DnsResponse::Nack(format!("Fidelity verification failed {e:?}")),
                    )?;
                }
            }
        }
        DnsRequest::Get => {
            log::info!("Received GET");

            let addresses = directory.addresses.read()?;

            let response = addresses
                .iter()
                .filter(|(_, (_, timestamp))| timestamp.elapsed() <= Duration::from_secs(30 * 60))
                .fold(String::new(), |acc, (_, addr)| acc + &addr.0 + "\n");

            log::debug!("Sending Addresses: {response}");
            send_message(stream, &response)?;
        }
        #[cfg(feature = "integration-test")]
        // Used for IT, only checks the updated_address_map() function.
        DnsRequest::Dummy { url, vout } => {
            use std::str::FromStr;
            log::info!("Got new maker address: {}", &url);

            // Create a constant txid for tests
            // It's okay to unwrap as this is test-only
            let txid = bitcoin::Txid::from_str(
                "c3a04e4bdf3c8684c5cf5c8b2f3c43009670bc194ac6c856b3ec9d3a7a6e2602",
            )
            .unwrap();
            let fidelity_op = OutPoint::new(txid, vout);

            directory.updated_address_map((url, fidelity_op))?;
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
            socks_port = 9050
        "#;
        create_temp_config(contents, &temp_dir);
        let dns = DirectoryServer::new(Some(temp_dir.path().to_path_buf()), None).unwrap();
        let default_dns = DirectoryServer::default();

        assert_eq!(dns.network_port, default_dns.network_port);
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

        assert_eq!(dns.network_port, 8080);
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

        assert_eq!(dns.network_port, default_dns.network_port);
        assert_eq!(dns.socks_port, default_dns.socks_port);

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_missing_file() {
        let temp_dir = TempDir::new().unwrap();
        let dns = DirectoryServer::new(Some(temp_dir.path().to_path_buf()), None).unwrap();
        let default_dns = DirectoryServer::default();

        assert_eq!(dns.network_port, default_dns.network_port);
        assert_eq!(dns.socks_port, default_dns.socks_port);

        temp_dir.close().unwrap();
    }
}

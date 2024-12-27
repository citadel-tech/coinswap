//! A simple directory-server
//!
//! Handles market-related logic where Makers post their offers. Also provides functions to synchronize
//! maker addresses from directory servers, post maker addresses to directory servers,

use bitcoind::bitcoincore_rpc::{self, Client, RpcApi};

use crate::{
    market::rpc::start_rpc_server_thread,
    utill::{
        get_dns_dir, parse_field, parse_toml, read_message, send_message, verify_fidelity_checks,
        ConnectionType, DnsRequest,
    },
    wallet::{RPCConfig, WalletError},
};

#[cfg(feature = "tor")]
use crate::utill::{get_tor_addrs, monitor_log_for_completion};

use std::{
    cmp::Ordering,
    collections::BTreeSet,
    convert::TryFrom,
    fs::{self, File},
    io::{BufRead, BufReader, Write},
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

/// Represents errors that can occur during directory server operations.
#[derive(Debug)]
pub enum DirectoryServerError {
    IO(std::io::Error),
    Net(NetError),
    MutexPossion,
    Wallet(WalletError),
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

#[derive(Debug, Eq, PartialEq)]
pub struct AddressEntry(pub u64, pub String);

impl PartialOrd for AddressEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for AddressEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        other.0.cmp(&self.0).then_with(|| self.1.cmp(&other.1))
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
    pub addresses: Arc<RwLock<BTreeSet<AddressEntry>>>,
}

impl Default for DirectoryServer {
    fn default() -> Self {
        Self {
            rpc_port: 4321,
            port: 8080,
            socks_port: 19060,
            connection_type: {
                #[cfg(feature = "tor")]
                {
                    ConnectionType::TOR
                }
                #[cfg(not(feature = "tor"))]
                {
                    ConnectionType::CLEARNET
                }
            },
            data_dir: get_dns_dir(),
            shutdown: AtomicBool::new(false),
            addresses: Arc::new(RwLock::new(BTreeSet::new())),
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

        let addresses = Arc::new(RwLock::new(BTreeSet::new()));
        let address_file = data_dir.join("addresses.dat");
        if let Ok(file) = File::open(&address_file) {
            let reader = BufReader::new(file);
            for address in reader.lines().map_while(Result::ok) {
                if let Some((key, value)) = address.split_once(',') {
                    if let Ok(key) = key.trim().parse::<u64>() {
                        addresses
                            .write()?
                            .insert(AddressEntry(key, value.to_string()));
                    }
                }
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

fn write_default_directory_config(config_path: &Path) -> Result<(), DirectoryServerError> {
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
        .map(|AddressEntry(addr, amount)| format!("{},{}\n", addr, amount))
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
    #[cfg(feature = "tor")]
    let mut tor_handle = None;

    let rpc_config = rpc_config.unwrap_or_default();

    let rpc_client = bitcoincore_rpc::Client::try_from(&rpc_config)?;

    match directory.connection_type {
        ConnectionType::CLEARNET => {}
        #[cfg(feature = "tor")]
        ConnectionType::TOR => {
            #[cfg(feature = "tor")]
            {
                let tor_log_dir = "/tmp/tor-rust-directory/log";
                if Path::new(tor_log_dir).exists() {
                    match fs::remove_file(tor_log_dir) {
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
                handle_client(&mut stream, &directory.clone(), &rpc_client)?;
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

    #[cfg(feature = "tor")]
    {
        if let Some(handle) = tor_handle {
            crate::tor::kill_tor_handles(handle);
            log::info!("Directory server and Tor instance terminated successfully");
        }
    }

    write_addresses_to_file(&directory, &address_file)?;

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
            log::info!("Received POST | From {}", &metadata.url);

            let txid = metadata.proof.bond.outpoint.txid;
            let transaction = rpc.get_raw_transaction(&txid, None)?;
            let current_height = rpc.get_block_count()?;

            match verify_fidelity_checks(
                &metadata.proof,
                &metadata.url,
                transaction,
                current_height,
            ) {
                Ok(_) => {
                    log::info!("Maker verified successfully.");
                    directory.addresses.write()?.insert(AddressEntry(
                        metadata.proof.bond.amount.to_sat(),
                        metadata.url.clone(),
                    ));
                }
                Err(e) => {
                    log::error!(
                        "Potentially suspicious maker detected: {:?} | {:?}",
                        metadata.url,
                        e
                    );
                }
            }
        }
        DnsRequest::Get => {
            log::info!("Received GET | From {}", stream.peer_addr()?);
            let addresses = directory.addresses.read()?;
            let response = addresses
                .iter()
                .fold(String::new(), |acc, AddressEntry(_, addr)| {
                    acc + addr + "\n"
                });
            log::debug!("Sending Addresses: {}", response);
            send_message(stream, &response)?;
        }
        #[cfg(feature = "integration-test")]
        DnsRequest::Dummy { url } => {
            log::info!("Got new maker address: {}", &url);
            directory.addresses.write()?.insert(AddressEntry(0, url));
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

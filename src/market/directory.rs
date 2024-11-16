//! A simple directory-server
//!
//! Handles market-related logic where Makers post their offers. Also provides functions to synchronize
//! maker addresses from directory servers, post maker addresses to directory servers,

use crate::{
    market::rpc::start_rpc_server_thread,
    utill::{
        get_dns_dir, get_tor_addrs, monitor_log_for_completion, parse_field, parse_toml,
        ConnectionType,
    },
};

use std::{
    collections::HashSet,
    fs::{File, OpenOptions},
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
///
/// Encapsulates directory server error types for IO, networking, and concurrency failures from external operations that can occur during operations.
#[derive(Debug)]
pub enum DirectoryServerError {
    IO(std::io::Error),
    /// Network errors during connection establishment, data transfer, or socket operations.
    Net(NetError),
    /// Threading error when a mutex is poisoned due to a thread panic while holding the lock.
    MutexPossion,
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

/// This struct is used to store configuration options for the directory server.
///
///  Handles both clearnet and Tor connections, maintains network addresses, and manages server lifecycle.
#[derive(Debug)]
pub struct DirectoryServer {
    /// Port for RPC server accepting remote management commands.
    pub rpc_port: u16,
    /// Main port for accepting client connections.
    pub port: u16,
    /// SOCKS proxy port when running in Tor mode.
    pub socks_port: u16,
    /// Determines if server runs in clearnet or Tor mode.
    pub connection_type: ConnectionType,
    /// Directory for persistent storage of addresses and logs.
    pub data_dir: PathBuf,
    /// Flag to signal server shutdown across threads.
    pub shutdown: AtomicBool,
    /// Thread-safe set of network addresses known to this server.
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
    /// Creates a new Directory Server with the given data directory and connection type.
    ///
    /// Configuration is loaded from config.toml in the data directory:
    /// - Uses provided data dir or defaults to ~/.coinswap/dns/
    /// - Creates default config if none exists (template in ./directory.toml)
    /// - Loads saved addresses from addresses.dat if present
    ///
    /// # Errors
    /// - IO errors when reading/writing config or address files
    /// - TOML parsing errors
    /// - Mutex poisoning when loading addresses
    pub fn new(
        data_dir: Option<PathBuf>,
        connection_type: Option<ConnectionType>,
    ) -> Result<Self, DirectoryServerError> {
        let default_config = Self::default();

        let data_dir = data_dir.unwrap_or(get_dns_dir());
        let config_path = data_dir.join("config.toml");

        // This will create parent directories if they don't exist
        if !config_path.exists() {
            write_default_directory_config(&config_path)?;
            log::warn!(
                "Directory config file not found, creating default config file at path: {}",
                config_path.display()
            );
        }

        let section = parse_toml(&config_path)?;
        log::info!(
            "Successfully loaded config file from : {}",
            config_path.display()
        );

        let directory_config_section = section.get("maker_config").cloned().unwrap_or_default();

        let connection_type_value = connection_type.unwrap_or(ConnectionType::TOR);

        let addresses = Arc::new(RwLock::new(HashSet::new()));
        let address_file = data_dir.join("addresses.dat");
        if let Ok(file) = File::open(&address_file) {
            let reader = BufReader::new(file);
            for address in reader.lines().map_while(Result::ok) {
                addresses.write()?.insert(address);
            }
        }

        Ok(DirectoryServer {
            rpc_port: 4321,
            port: parse_field(directory_config_section.get("port"), default_config.port)
                .unwrap_or(default_config.port),
            socks_port: parse_field(
                directory_config_section.get("socks_port"),
                default_config.socks_port,
            )
            .unwrap_or(default_config.socks_port),
            data_dir,
            shutdown: AtomicBool::new(false),
            connection_type: parse_field(
                directory_config_section.get("connection_type"),
                connection_type_value,
            )
            .unwrap_or(connection_type_value),
            addresses,
        })
    }
    /// Signals the server to shut down gracefully.
    ///
    /// Sets the atomic shutdown flag that triggers cleanup in the main server loop.
    pub fn shutdown(&self) -> Result<(), DirectoryServerError> {
        self.shutdown.store(true, Relaxed);
        Ok(())
    }
}

fn write_default_directory_config(config_path: &PathBuf) -> Result<(), DirectoryServerError> {
    let config_string = String::from(
        "\
            [directory_config]\n\
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
/// Runs the address persistence loop with configurable interval.
///
/// Periodically writes the directory's address state to disk:
/// - Uses 10 minute intervals in production
/// - Uses 1 minute intervals in integration-test
/// - Logs any write errors
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

/// Writes the directory's current address list to disk.
///
/// Opens or creates the address file and:
/// - Formats each address on a new line
/// - Truncates existing content
/// - Ensures write is flushed to disk
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

    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(
            address_file
                .to_str()
                .expect("address file path must exist at this stage"),
        )?;

    file.write_all(file_content.as_bytes())?;
    file.flush()?;
    Ok(())
}
/// Starts the directory server with optional Tor support, RPC server, and address persistence.
///
/// Spawns the main TCP listener and supporting threads:
/// - Tor daemon (optional: requires ConnectionType::TOR and 'tor' feature)
/// - RPC server for remote management
/// - Address writer for state persistence
///
/// # Shutdown
/// Server runs until shutdown flag is set, then:
/// - Joins all threads
/// - Persists final address state
/// - Terminates Tor if running
pub fn start_directory_server(directory: Arc<DirectoryServer>) -> Result<(), DirectoryServerError> {
    let mut tor_handle = None;

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

                let onion_addr = get_tor_addrs(&PathBuf::from("/tmp/tor-rust-directory"));

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
                stream.set_read_timeout(Some(Duration::from_secs(20)))?;
                stream.set_write_timeout(Some(Duration::from_secs(20)))?;
                handle_client(&mut stream, &directory.clone())?;
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
// TODO: Use serde encoded data instead of string.
fn handle_client(
    stream: &mut TcpStream,
    directory: &Arc<DirectoryServer>,
) -> Result<(), DirectoryServerError> {
    let reader_stream = stream.try_clone()?;
    let mut reader = BufReader::new(reader_stream);
    let mut request_line = String::new();

    reader.read_line(&mut request_line)?;
    if request_line.starts_with("POST") {
        let addr: String = request_line.replace("POST ", "").trim().to_string();
        directory.addresses.write()?.insert(addr.clone());
        log::info!("Got new maker address: {}", addr);
    } else if request_line.starts_with("GET") {
        log::info!("Taker pinged the directory server");
        let response = directory
            .addresses
            .read()?
            .iter()
            .fold(String::new(), |acc, addr| acc + addr + "\n");
        stream.write_all(response.as_bytes())?;
        stream.flush()?;
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
        let config = DirectoryServer::new(Some(temp_dir.path().to_path_buf()), None).unwrap();
        let default_config = DirectoryServer::default();

        assert_eq!(config.port, default_config.port);
        assert_eq!(config.socks_port, default_config.socks_port);

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
        let config = DirectoryServer::new(Some(temp_dir.path().to_path_buf()), None).unwrap();

        assert_eq!(config.port, 8080);
        assert_eq!(config.socks_port, DirectoryServer::default().socks_port);

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
        let config = DirectoryServer::new(Some(temp_dir.path().to_path_buf()), None).unwrap();
        let default_config = DirectoryServer::default();

        assert_eq!(config.port, default_config.port);
        assert_eq!(config.socks_port, default_config.socks_port);

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_missing_file() {
        let temp_dir = TempDir::new().unwrap();
        let config = DirectoryServer::new(Some(temp_dir.path().to_path_buf()), None).unwrap();
        let default_config = DirectoryServer::default();

        assert_eq!(config.port, default_config.port);
        assert_eq!(config.socks_port, default_config.socks_port);

        temp_dir.close().unwrap();
    }
}

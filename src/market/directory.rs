//! A simple directory-server
//!
//! Handles market-related logic where Makers post their offers. Also provides functions to synchronize
//! maker addresses from directory servers, post maker addresses to directory servers,

use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::{self, BufRead, BufReader, Write},
    net::{Ipv4Addr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    thread::{self, sleep},
    time::Duration,
};

use crate::{
    market::rpc::start_rpc_server_thread,
    utill::{
        get_dns_dir, get_tor_addrs, monitor_log_for_completion, parse_field, parse_toml,
        write_default_config, ConnectionType,
    },
};

/// Represents errors that can occur during directory server operations.
#[derive(Debug)]
pub enum DirectoryServerError {
    Other(&'static str),
}

/// Directory Configuration,
#[derive(Debug)]
pub struct DirectoryServer {
    pub rpc_port: u16,
    pub port: u16,
    pub socks_port: u16,
    pub connection_type: ConnectionType,
    pub data_dir: PathBuf,
    pub shutdown: RwLock<bool>,
}

impl Default for DirectoryServer {
    fn default() -> Self {
        Self {
            rpc_port: 4321,
            port: 8080,
            socks_port: 19060,
            connection_type: ConnectionType::TOR,
            data_dir: get_dns_dir(),
            shutdown: RwLock::new(false),
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
    /// Default data-dir for linux: `~/.coinswap/`
    /// Default config locations: `~/.coinswap/dns/config.toml`.
    pub fn new(
        data_dir: Option<PathBuf>,
        connection_type: Option<ConnectionType>,
    ) -> io::Result<Self> {
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
            shutdown: RwLock::new(false),
            connection_type: parse_field(
                directory_config_section.get("connection_type"),
                connection_type_value,
            )
            .unwrap_or(connection_type_value),
        })
    }

    pub fn shutdown(&self) -> Result<(), DirectoryServerError> {
        let mut flag = self
            .shutdown
            .write()
            .map_err(|_| DirectoryServerError::Other("Rwlock write error!"))?;
        *flag = true;
        Ok(())
    }
}

fn write_default_directory_config(config_path: &PathBuf) -> std::io::Result<()> {
    let config_string = String::from(
        "\
            [directory_config]\n\
            port = 8080\n\
            socks_port = 19060\n\
            connection_type = tor\n\
            rpc_port= 4321\n\
            ",
    );

    write_default_config(config_path, config_string)
}

pub fn start_directory_server(directory: Arc<DirectoryServer>) {
    let address_file = directory.data_dir.join("addresses.dat");

    let addresses = Arc::new(RwLock::new(HashSet::new()));

    let mut tor_handle = None;

    match directory.connection_type {
        ConnectionType::CLEARNET => {}
        ConnectionType::TOR => {
            if cfg!(feature = "tor") {
                let tor_log_dir = "/tmp/tor-rust-directory/log".to_string();
                if Path::new(tor_log_dir.as_str()).exists() {
                    match fs::remove_file(Path::new(tor_log_dir.clone().as_str())) {
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

                thread::sleep(Duration::from_secs(10));

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

    let directory_server_arc = directory.clone();
    let addres_arc = addresses.clone();
    let rpc_thread = thread::spawn(|| {
        start_rpc_server_thread(directory_server_arc, addres_arc);
    });

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, directory.port)).unwrap();

    while !*directory.shutdown.read().unwrap() {
        match listener.accept() {
            Ok((mut stream, addrs)) => {
                log::debug!("Incoming connection from : {}", addrs);
                let address_arc = addresses.clone();
                stream
                    .set_read_timeout(Some(Duration::from_secs(20)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(20)))
                    .unwrap();
                handle_client(&mut stream, address_arc);
            }

            // If no connection received, check for shutdown or save addresses to disk
            Err(e) => {
                log::error!("Error accepting incoming connection: {:?}", e);
            }
        }

        sleep(Duration::from_secs(3));
    }

    log::info!("Shutdown signal received. Stopping directory server.");
    rpc_thread.join().unwrap();
    if let Some(handle) = tor_handle {
        crate::tor::kill_tor_handles(handle);
        log::info!("Directory server and Tor instance terminated successfully");
    }

    // Write the addresses to file
    let file_content = addresses
        .read()
        .unwrap()
        .iter()
        .map(|addr| format!("{}\n", addr))
        .collect::<Vec<String>>()
        .join("");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true) // Override the address file
        .open(address_file.to_str().unwrap())
        .unwrap();
    file.write_all(file_content.as_bytes()).unwrap();
    file.flush().unwrap();
}

// The stream should have read and write timeout set.
// TODO: Use serde encoded data instead of string.
fn handle_client(stream: &mut TcpStream, addresses: Arc<RwLock<HashSet<String>>>) {
    let reader_stream = stream.try_clone().unwrap();
    let mut reader = BufReader::new(reader_stream);
    let mut request_line = String::new();

    reader.read_line(&mut request_line).unwrap();

    if request_line.starts_with("POST") {
        let addr: String = request_line.replace("POST ", "").trim().to_string();
        addresses.write().unwrap().insert(addr.clone());
        log::info!("Got new maker address: {}", addr);
    } else if request_line.starts_with("GET") {
        log::info!("Taker pinged the directory server");
        let response = addresses
            .read()
            .unwrap()
            .iter()
            .fold(String::new(), |acc, addr| acc + addr + "\n");
        stream.write_all(response.as_bytes()).unwrap();
        stream.flush().unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoind::tempfile::TempDir;

    fn create_temp_config(contents: &str, temp_dir: &TempDir) -> PathBuf {
        let config_path = temp_dir.path().join("config.toml");
        fs::write(&config_path, contents).unwrap();
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

//! A simple directory-server
//!
//! Handles market-related logic where Makers post their offers. Also provides functions to synchronize
//! maker addresses from directory servers, post maker addresses to directory servers,

use std::{
    collections::HashSet,
    fs, io,
    net::Ipv4Addr,
    sync::{Arc, RwLock},
    thread,
    time::Duration,
};

use std::path::{Path, PathBuf};

use crate::utill::{
    get_data_dir, monitor_log_for_completion, parse_field, parse_toml, write_default_config,
    ConnectionType,
};

use tokio::{
    fs::OpenOptions,
    io::{AsyncBufReadExt, AsyncWriteExt},
    net::TcpListener,
};

/// Represents errors that can occur during directory server operations.
#[derive(Debug)]
pub enum DirectoryServerError {
    Reqwest(reqwest::Error),
    Other(&'static str),
}

/// Directory Configuration,
#[derive(Debug)]
pub struct DirectoryServer {
    pub port: u16,
    pub socks_port: u16,
    pub connection_type: ConnectionType,
    pub shutdown: RwLock<bool>,
}

impl Default for DirectoryServer {
    fn default() -> Self {
        Self {
            port: 8080,
            socks_port: 19060,
            connection_type: ConnectionType::TOR,
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
    /// Default config locations: `~/.coinswap/directory_server/configs/directory.toml`.

    pub fn new(
        config_path: Option<&PathBuf>,
        connection_type: Option<ConnectionType>,
    ) -> io::Result<Self> {
        let default_config = Self::default();

        let default_config_path = get_data_dir()
            .join("directory_server")
            .join("config")
            .join("directory.toml");
        let config_path = config_path.unwrap_or(&default_config_path);

        if !config_path.exists() {
            write_default_directory_config(config_path);
            log::warn!(
                "Directory config file not found, creating default config file at path: {}",
                config_path.display()
            );
        }

        let section = parse_toml(config_path)?;
        log::info!(
            "Successfully loaded config file from : {}",
            config_path.display()
        );

        let directory_config_section = section.get("maker_config").cloned().unwrap_or_default();

        let connection_type_value = connection_type.unwrap_or(ConnectionType::TOR);

        Ok(DirectoryServer {
            port: parse_field(directory_config_section.get("port"), default_config.port)
                .unwrap_or(default_config.port),
            socks_port: parse_field(
                directory_config_section.get("socks_port"),
                default_config.socks_port,
            )
            .unwrap_or(default_config.socks_port),
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

fn write_default_directory_config(config_path: &PathBuf) {
    let config_string = String::from(
        "\
            [directory_config]\n\
            port = 8080\n\
            socks_port = 19060\n\
            connection_type = tor\n\
            ",
    );

    write_default_config(config_path, config_string).unwrap();
}

impl From<reqwest::Error> for DirectoryServerError {
    fn from(e: reqwest::Error) -> DirectoryServerError {
        DirectoryServerError::Reqwest(e)
    }
}

#[tokio::main]
pub async fn start_directory_server(directory: Arc<DirectoryServer>) {
    log::info!("Inside Directory Server");

    let address_file = get_data_dir().join("directory_server").join("address.dat");

    let mut addresses = HashSet::new();

    let mut handle = None;

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
                handle = Some(crate::tor::spawn_tor(
                    socks_port,
                    tor_port,
                    "/tmp/tor-rust-directory".to_string(),
                ));

                thread::sleep(Duration::from_secs(10));

                if let Err(e) = monitor_log_for_completion(PathBuf::from(tor_log_dir), "100%") {
                    log::error!("Error monitoring Directory log file: {}", e);
                }

                log::info!("Directory tor is instantiated");
            }
        }
    }

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, directory.port))
        .await
        .unwrap();

    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, _)) => handle_client(stream, &mut addresses).await,
                    Err(e) => log::error!("Error accepting connection: {}", e),
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(3)) => {
                if *directory.shutdown.read().unwrap() {
                    log::info!("Shutdown signal received. Stopping directory server.");
                    if directory.connection_type == ConnectionType::TOR && cfg!(feature = "tor"){
                        crate::tor::kill_tor_handles(handle.unwrap());
                        log::info!("Directory server and Tor instance terminated successfully");
                    }
                    break;
                } else {
                     let mut file = OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(address_file.to_str().unwrap())
                    .await
                    .unwrap();
                    for addr in &addresses {
                        let content = format!("{}\n", addr);
                        file.write_all(content.as_bytes()).await.unwrap();
                    }
                }
            }
        }
    }
}

async fn handle_client(mut stream: tokio::net::TcpStream, addresses: &mut HashSet<String>) {
    let mut reader = tokio::io::BufReader::new(&mut stream);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).await.unwrap();

    if request_line.starts_with("POST") {
        let onion_address = request_line.replace("POST ", "").trim().to_string();
        addresses.insert(onion_address.clone());
        log::info!("Got new maker address: {}", onion_address);
    } else if request_line.starts_with("GET") {
        log::info!("Taker pinged the directory server");
        let response = addresses
            .iter()
            .fold(String::new(), |acc, addr| acc + addr + "\n");
        stream.write_all(response.as_bytes()).await.unwrap();
    }
}

#[cfg(test)]
mod tests {
    use crate::utill::get_home_dir;

    use super::*;
    use std::{fs::File, io::Write};

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
    fn test_valid_config() {
        let contents = r#"
            [directory_config]
            port = 8080
            socks_port = 19060
        "#;
        let config_path = create_temp_config(contents, "valid_directory_config.toml");
        let config = DirectoryServer::new(Some(&config_path), None).unwrap();
        remove_temp_config(&config_path);

        let default_config = DirectoryServer::default();
        assert_eq!(config.port, default_config.port);
        assert_eq!(config.socks_port, default_config.socks_port);
    }

    #[test]
    fn test_missing_fields() {
        let contents = r#"
            [directory_config]
            port = 8080
        "#;
        let config_path = create_temp_config(contents, "missing_fields_directory_config.toml");
        let config = DirectoryServer::new(Some(&config_path), None).unwrap();
        remove_temp_config(&config_path);

        assert_eq!(config.port, 8080);
        assert_eq!(config.socks_port, DirectoryServer::default().socks_port);
    }

    #[test]
    fn test_incorrect_data_type() {
        let contents = r#"
            [directory_config]
            port = "not_a_number"
        "#;
        let config_path = create_temp_config(contents, "incorrect_type_directory_config.toml");
        let config = DirectoryServer::new(Some(&config_path), None).unwrap();
        remove_temp_config(&config_path);

        let default_config = DirectoryServer::default();
        assert_eq!(config.port, default_config.port);
        assert_eq!(config.socks_port, default_config.socks_port);
    }

    #[test]
    fn test_missing_file() {
        let config_path = get_home_dir().join("directory.toml");
        let config = DirectoryServer::new(Some(&config_path), None).unwrap();
        remove_temp_config(&config_path);
        let default_config = DirectoryServer::default();
        assert_eq!(config.port, default_config.port);
        assert_eq!(config.socks_port, default_config.socks_port);
    }
}

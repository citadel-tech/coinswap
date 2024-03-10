//! A simple directory-server
//!
//! Handles market-related logic where Makers post their offers. Also provides functions to synchronize
//! maker addresses from directory servers, post maker addresses to directory servers,
//! and defines constants such as Tor addresses and directory server addresses.

/// Represents the Tor address and port configuration.
// It should be set to your specific Tor address and port.
pub const TOR_SOCKS_ADDR: &str = "127.0.0.1:19050";

use std::{
    collections::HashSet,
    fs,
    sync::{Arc, RwLock},
    thread,
    time::Duration,
};

use std::path::{Path, PathBuf};

use crate::utill::{kill_tor_handles, monitor_log_for_completion, spawn_tor};

use tokio::{
    fs::OpenOptions,
    io::{AsyncBufReadExt, AsyncWriteExt},
    net::TcpListener,
};

const ADDRESS_FILE: &str = "/tmp/maker_addresses.dat";

/// Represents errors that can occur during directory server operations.
#[derive(Debug)]
pub enum DirectoryServerError {
    Reqwest(reqwest::Error),
    Other(&'static str),
}

pub struct DirectoryServer {
    pub port: u16,
    pub socks_port: u16,
    pub shutdown: RwLock<bool>,
}

impl DirectoryServer {
    pub fn init(port: Option<u16>, socks_port: Option<u16>) -> Result<Self, DirectoryServerError> {
        Ok(Self {
            port: port.unwrap_or(8080),
            socks_port: socks_port.unwrap_or(19060),
            shutdown: RwLock::new(false),
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

impl From<reqwest::Error> for DirectoryServerError {
    fn from(e: reqwest::Error) -> DirectoryServerError {
        DirectoryServerError::Reqwest(e)
    }
}

#[tokio::main]
pub async fn start_directory_server(directory: Arc<DirectoryServer>) {
    log::info!("Inside Directory Server");

    let tor_log_dir = "/tmp/tor-rust-directory/log".to_string();
    if Path::new(tor_log_dir.as_str()).exists() {
        match fs::remove_file(Path::new(tor_log_dir.clone().as_str())) {
            Ok(_) => log::info!("Previous directory log file deleted successfully"),
            Err(_) => log::error!("Error deleting directory log file"),
        }
    }

    let socks_port = directory.socks_port;
    let tor_port = directory.port;
    let handle = spawn_tor(socks_port, tor_port, "/tmp/tor-rust-directory".to_string());

    let mut addresses = HashSet::new();

    thread::sleep(Duration::from_secs(10));

    if let Err(e) = monitor_log_for_completion(PathBuf::from(tor_log_dir), "100%") {
        log::error!("Error monitoring Directory log file: {}", e);
    }

    log::info!("Directory tor is instantiated");

    let listener = TcpListener::bind("127.0.0.1:8080").await.unwrap();

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
                   kill_tor_handles(handle);
                    log::info!("Directory server and Tor instance terminated successfully");
                    break;
                } else {
                     let mut file = OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(ADDRESS_FILE)
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
        log::info!("Maker pinged the directory server");
        let onion_address = request_line.replace("POST ", "").trim().to_string();
        addresses.insert(onion_address.clone());
    } else if request_line.starts_with("GET") {
        log::warn!("Taker pinged the directory server");
        let response = addresses
            .iter()
            .fold(String::new(), |acc, addr| acc + addr + "\n");
        stream.write_all(response.as_bytes()).await.unwrap();
    }
}

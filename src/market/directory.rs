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

use bitcoin::Network;
use std::path::{Path, PathBuf};

use crate::{taker::offers::MakerAddress, utill::monitor_log_for_completion};
use libtor::{HiddenServiceVersion, LogDestination, LogLevel, Tor, TorAddress, TorFlag};

use tokio::{
    fs::OpenOptions,
    io::{AsyncBufReadExt, AsyncWriteExt},
    net::TcpListener,
};

//for now just one of these, but later we'll need multiple for good decentralization
const DIRECTORY_SERVER_ADDR: &str =
    "pl62q4gupqgzkyunif5kudjwyt2oelikpt5pkw5bnvy2wrm6luog2dad.onion:8000";

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

/// Converts a `Network` enum variant to its corresponding string representation.
fn network_enum_to_string(network: Network) -> &'static str {
    match network {
        Network::Bitcoin => "mainnet",
        Network::Testnet => "testnet",
        Network::Signet => "signet",
        Network::Regtest => panic!("dont use directory servers if using regtest"),
        _ => todo!(),
    }
}
/// Asynchronously Synchronize Maker Addresses from Directory Servers.
pub async fn sync_maker_addresses_from_directory_servers(
    network: Network,
) -> Result<Vec<MakerAddress>, DirectoryServerError> {
    // https://github.com/seanmonstar/reqwest/blob/master/examples/tor_socks.rs
    let proxy = reqwest::Proxy::all(format!("socks5h://{}", TOR_SOCKS_ADDR))
        .expect("tor proxy should be there");
    let client = reqwest::Client::builder()
        .proxy(proxy)
        .build()
        .expect("should be able to build reqwest client");
    let res = client
        .get(format!(
            "http://{}/makers-{}.txt",
            DIRECTORY_SERVER_ADDR,
            network_enum_to_string(network)
        ))
        .send()
        .await?;
    if res.status().as_u16() != 200 {
        return Err(DirectoryServerError::Other("status code not success"));
    }
    let mut maker_addresses = Vec::<MakerAddress>::new();
    for makers in res.text().await?.split('\n') {
        let csv_chunks = makers.split(',').collect::<Vec<&str>>();
        if csv_chunks.len() < 2 {
            continue;
        }
        maker_addresses.push(MakerAddress::new(String::from(csv_chunks[1])));
        log::debug!(target:"directory_servers", "expiry timestamp = {} address = {}",
            csv_chunks[0], csv_chunks[1]);
    }
    Ok(maker_addresses)
}

/// Posts a maker's address to directory servers based on the specified network.
pub async fn post_maker_address_to_directory_servers(
    network: Network,
    address: &str,
) -> Result<u64, DirectoryServerError> {
    let proxy = reqwest::Proxy::all(format!("socks5h://{}", TOR_SOCKS_ADDR))
        .expect("tor proxy should be there");
    let client = reqwest::Client::builder()
        .proxy(proxy)
        .build()
        .expect("should be able to build reqwest client");
    let params = [
        ("address", address),
        ("net", network_enum_to_string(network)),
    ];
    let res = client
        .post(format!("http://{}/directoryserver", DIRECTORY_SERVER_ADDR))
        .form(&params)
        .send()
        .await?;
    if res.status().as_u16() != 200 {
        return Err(DirectoryServerError::Other("status code not success"));
    }
    let body = res.text().await?;
    let start_bytes = body
        .find("<b>")
        .ok_or(DirectoryServerError::Other("expiry time not parsable1"))?
        + 3;
    let end_bytes = body
        .find("</b>")
        .ok_or(DirectoryServerError::Other("expiry time not parsable2"))?;
    let expiry_time_str = &body[start_bytes..end_bytes];
    let expiry_time = expiry_time_str
        .parse::<u64>()
        .map_err(|_| DirectoryServerError::Other("expiry time not parsable3"))?;
    Ok(expiry_time)
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

    thread::sleep(Duration::from_secs(10));
    let socks_port = directory.socks_port;
    let tor_port = directory.port;
    let handle = mitosis::spawn((tor_port, socks_port), |(tor_port, socks_port)| {
        let hs_string = "/tmp/tor-rust-directory/hs-dir".to_string();
        let data_dir = "/tmp/tor-rust-directory".to_string();
        let log_dir = "/tmp/tor-rust-directory/log".to_string();
        let directory_file_path = PathBuf::from(data_dir.as_str());
        if !directory_file_path.exists() {
            std::fs::create_dir_all(directory_file_path).unwrap();
        }
        let _handler = Tor::new()
            .flag(TorFlag::DataDirectory(data_dir))
            .flag(TorFlag::LogTo(
                LogLevel::Notice,
                LogDestination::File(log_dir),
            ))
            .flag(TorFlag::SocksPort(socks_port))
            .flag(TorFlag::HiddenServiceDir(hs_string))
            .flag(TorFlag::HiddenServiceVersion(HiddenServiceVersion::V3))
            .flag(TorFlag::HiddenServicePort(
                TorAddress::Port(tor_port),
                None.into(),
            ))
            .start();
    });

    let mut addresses = HashSet::new();

    if Path::new(ADDRESS_FILE).exists() {
        match fs::remove_file(Path::new(ADDRESS_FILE)) {
            Ok(_) => log::info!("Previous directory address data file deleted successfully"),
            Err(_) => log::error!("Error deleting directory address data file"),
        }
    }

    thread::sleep(Duration::from_secs(10));

    if let Err(e) = monitor_log_for_completion(PathBuf::from(tor_log_dir), "100%").await {
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
            _ = tokio::time::sleep(Duration::from_secs(1)) => {
                if *directory.shutdown.read().unwrap() {
                    log::info!("Shutdown signal received. Stopping directory server.");
                    match handle.kill() {
                        Ok(_) => log::info!("Tor instance terminated successfully"),
                        Err(_) => log::error!("Error occurred while terminating tor instance"),
                    }
                    log::info!("Directory server and Tor instance terminated successfully");
                    break;
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
        log::warn!("Maker pinged the directory server");
        let onion_address = request_line.replace("POST ", "").trim().to_string();
        addresses.insert(onion_address.clone());
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(ADDRESS_FILE)
            .await
            .unwrap();
        let content = format!("{}\n", onion_address);
        file.write_all(content.as_bytes()).await.unwrap();
    } else if request_line.starts_with("GET") {
        log::warn!("Taker pinged the directory server");
        let response = addresses
            .iter()
            .fold(String::new(), |acc, addr| acc + addr + "\n");
        stream.write_all(response.as_bytes()).await.unwrap();
    }
}

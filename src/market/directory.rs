//! A simple directory-server
//!
//! Handles market-related logic where Makers post their offers. Also provides functions to synchronize
//! maker addresses from directory servers, post maker addresses to directory servers,
//! and defines constants such as Tor addresses and directory server addresses.

/// Represents the Tor address and port configuration.
// It should be set to your specific Tor address and port.
pub const TOR_SOCKS_ADDR: &str = "127.0.0.1:19050";

use std::{collections::HashSet, thread, time::Duration};

use bitcoin::Network;
use std::path::PathBuf;

use crate::taker::offers::MakerAddress;
use libtor::{HiddenServiceVersion, Tor, TorAddress, TorFlag};
use std::path::Path;

use tokio::fs::File;
use tokio::fs::OpenOptions;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

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
pub async fn start_directory_server(shutdown_rx: oneshot::Receiver<()>) {
    log::info!("Inside Directory Server");

    let handle = mitosis::spawn(19060, |_data| {
        let hs_string = format!("/tmp/tor-rust-directory/hs-dir");
        let data_dir = format!("/tmp/tor-rust-directory");
        let directory_file_path = PathBuf::from(data_dir.as_str());
        if !directory_file_path.exists() {
            std::fs::create_dir_all(directory_file_path).unwrap();
        }
        let _handler = Tor::new()
            .flag(TorFlag::DataDirectory(data_dir))
            .flag(TorFlag::SocksPort(19060))
            .flag(TorFlag::HiddenServiceDir(hs_string))
            .flag(TorFlag::HiddenServiceVersion(HiddenServiceVersion::V3))
            .flag(TorFlag::HiddenServicePort(
                TorAddress::Port(8080),
                None.into(),
            ))
            .start();
    });

    thread::sleep(Duration::from_secs(60));
    log::info!("Wake up from tor directory Server");

    let mut addresses = HashSet::new();
    if Path::new(ADDRESS_FILE).exists() {
        let file = File::open(ADDRESS_FILE).await.unwrap();
        let mut reader = BufReader::new(file);
        let mut line = String::new();

        while reader.read_line(&mut line).await.unwrap() > 0 {
            addresses.insert(line.clone().trim().to_string());
            line.clear();
        }
    }

    let listener = TcpListener::bind("127.0.0.1:8080").await.unwrap();

    tokio::select! {
        _ = serve(listener, &mut addresses) => {},
        _ = shutdown_rx => {
            log::info!("Shutdown signal received.");
            match handle.kill() {
                Ok(_) => log::info!("Tor instance terminated successfully"),
                Err(_) => log::error!("Error occurred while terminating tor instance"),
            }
            log::info!("Directory server and Tor instance terminated successfully");
        },
    }
}

async fn serve(listener: TcpListener, addresses: &mut HashSet<String>) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                handle_client(stream, addresses).await;
            }
            Err(e) => {
                println!("Error accepting connection: {}", e);
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
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(ADDRESS_FILE)
            .await
            .unwrap();
        let content = format!("{}\n", onion_address);
        file.write_all(content.as_bytes()).await.unwrap();
    } else if request_line.starts_with("GET") {
        let response = addresses
            .iter()
            .fold(String::new(), |acc, addr| acc + addr + "\n");
        stream.write_all(response.as_bytes()).await.unwrap();
    }
}

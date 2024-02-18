//! Download, process and store Maker offers from the directory-server.
//!
//! It defines structures like [OfferAndAddress] and [MakerAddress] for representing maker offers and addresses.
//! The [OfferBook] struct keeps track of good and bad makers, and it provides methods for managing offers.
//! The module handles the syncing of the offer book with addresses obtained from directory servers and local configurations.
//! It uses asynchronous channels for concurrent processing of maker offers.

use std::{ fmt, fs::File, io::Read, path::PathBuf };

use tokio::{ io::{ AsyncReadExt, AsyncWriteExt }, net::TcpStream, sync::mpsc };

use bitcoin::Network;

use crate::protocol::messages::Offer;

use crate::market::directory::DirectoryServerError;

use super::{ config::TakerConfig, error::TakerError, routines::download_maker_offer };
use tokio_socks::tcp::Socks5Stream;

/// Represents an offer along with the corresponding maker address.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Hash)]
pub struct OfferAndAddress {
    pub offer: Offer,
    pub address: MakerAddress,
}

const _REGTEST_MAKER_ADDRESSES_PORT: &[&str] = &["6102", "16102", "26102", "36102", "46102"];

type OnionAddress = String;
/// Enum representing maker addresses.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MakerAddress(OnionAddress);

impl MakerAddress {
    /// Returns the TCP stream address as a string.
    pub fn get_tcpstream_address(&self) -> String {
        self.0.to_string()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn new(address: String) -> Self {
        MakerAddress(address)
    }
}

impl fmt::Display for MakerAddress {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// An ephemeral Offerbook tracking good and bad makers. Currently, Offerbook is initiated
/// at start of every swap. So good and bad maker list will ot be persisted.
// TODO: Persist the offerbook in disk.
#[derive(Debug, Default)]
pub struct OfferBook {
    pub(super) all_makers: Vec<OfferAndAddress>,
    pub(super) good_makers: Vec<OfferAndAddress>,
    pub(super) bad_makers: Vec<OfferAndAddress>,
}

impl OfferBook {
    /// Gets all untried offers.
    pub fn get_all_untried(&self) -> Vec<&OfferAndAddress> {
        self.all_makers
            .iter()
            .filter(|offer| !self.good_makers.contains(offer) && !self.bad_makers.contains(offer))
            .collect()
    }

    /// Adds a new offer to the offer book.
    pub fn add_new_offer(&mut self, offer: &OfferAndAddress) -> bool {
        if !self.all_makers.contains(offer) {
            self.all_makers.push(offer.clone());
            true
        } else {
            false
        }
    }

    /// Adds a good maker to the offer book.
    pub fn add_good_maker(&mut self, good_maker: &OfferAndAddress) -> bool {
        if !self.good_makers.contains(good_maker) {
            self.good_makers.push(good_maker.clone());
            true
        } else {
            false
        }
    }

    /// Adds a bad maker to the offer book.
    pub fn add_bad_maker(&mut self, bad_maker: &OfferAndAddress) -> bool {
        if !self.bad_makers.contains(bad_maker) {
            self.bad_makers.push(bad_maker.clone());
            true
        } else {
            false
        }
    }

    /// Synchronizes the offer book with addresses obtained from directory servers and local configurations.
    pub async fn sync_offerbook(
        &mut self,
        network: Network,
        config: &TakerConfig
    ) -> Result<Vec<OfferAndAddress>, TakerError> {
        let offers = sync_offerbook_with_addresses(
            get_advertised_maker_addresses(
                Some(config.socks_port),
                Some(config.directory_server_onion_address.clone()),
                network
            ).await?,
            config
        ).await;

        let new_offers = offers
            .into_iter()
            .filter(|offer| !self.bad_makers.contains(offer))
            .collect::<Vec<_>>();

        new_offers.iter().for_each(|offer| {
            self.add_new_offer(offer);
        });

        Ok(new_offers)
    }

    /// Gets the list of bad makers.
    pub fn get_bad_makers(&self) -> Vec<&OfferAndAddress> {
        self.bad_makers.iter().collect()
    }
}

async fn _get_regtest_maker_addresses() -> Vec<MakerAddress> {
    _REGTEST_MAKER_ADDRESSES_PORT
        .iter()
        .filter(|port| {
            let hs_path_str = format!("/tmp/tor-rust{}/maker/hs-dir/hostname", port);
            let hs_path = PathBuf::from(hs_path_str);
            hs_path.exists()
        })
        .map(|h| {
            let hs_path_str = format!("/tmp/tor-rust{}/maker/hs-dir/hostname", h);
            let hs_path = PathBuf::from(hs_path_str);
            let mut file = File::open(hs_path).unwrap();
            let mut onion_addr: String = String::new();
            file.read_to_string(&mut onion_addr).unwrap();
            onion_addr.pop();
            MakerAddress(format!("{}:{}", onion_addr, h))
        })
        .collect::<Vec<MakerAddress>>()
}

/// Synchronizes the offer book with specific maker addresses.
pub async fn sync_offerbook_with_addresses(
    maker_addresses: Vec<MakerAddress>,
    config: &TakerConfig
) -> Vec<OfferAndAddress> {
    let (offers_writer_m, mut offers_reader) = mpsc::channel::<Option<OfferAndAddress>>(100);
    //unbounded_channel makes more sense here, but results in a compile
    //error i cant figure out
    let maker_addresses_len = maker_addresses.len();
    for addr in maker_addresses {
        let offers_writer = offers_writer_m.clone();
        let taker_config: TakerConfig = config.clone();
        tokio::spawn(async move {
            let offer = download_maker_offer(addr, taker_config).await;
            log::debug!("Received Maker Offer: {:?}", offer);
            offers_writer.send(offer).await.unwrap();
        });
    }
    let mut result = Vec::<OfferAndAddress>::new();
    for _ in 0..maker_addresses_len {
        if let Some(offer_addr) = offers_reader.recv().await.unwrap() {
            result.push(offer_addr);
        }
    }
    result
}

/// Retrieves advertised maker addresses from directory servers based on the specified network.
pub async fn get_advertised_maker_addresses(
    socks_port: Option<u16>,
    directory_server_address: Option<String>,
    _network: Network
) -> Result<Vec<MakerAddress>, DirectoryServerError> {
    let mut directory_onion_address = match directory_server_address {
        Some(address) => address,
        None => "".to_string(),
    };
    if cfg!(feature = "integration-test") {
        let directory_hs_path_str = "/tmp/tor-rust-directory/hs-dir/hostname".to_string();
        let directory_hs_path = PathBuf::from(directory_hs_path_str);
        let mut directory_file = tokio::fs::File
            ::open(&directory_hs_path).await
            .map_err(|_e| DirectoryServerError::Other("Directory hidden service path not found"))?;
        let mut directory_onion_addr = String::new();
        directory_file
            .read_to_string(&mut directory_onion_addr).await
            .map_err(|_e| DirectoryServerError::Other("Reading onion address failed"))?;
        directory_onion_addr.pop();
        directory_onion_address = format!("{}:{}", directory_onion_addr, 8080);
    }
    let mut stream: TcpStream = Socks5Stream::connect(
        format!("127.0.0.1:{}", socks_port.unwrap_or(19050)).as_str(),
        directory_onion_address.as_str()
    ).await
        .map_err(|_e| {
            DirectoryServerError::Other("Issue with fetching maker address from directory server")
        })?
        .into_inner();
    let request_line = "GET\n";
    stream
        .write_all(request_line.as_bytes()).await
        .map_err(|_e| DirectoryServerError::Other("Error sending the request"))?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response).await
        .map_err(|_e| DirectoryServerError::Other("Error receiving the response"))?;
    log::warn!("Received: {}", response);
    let addresses: Vec<MakerAddress> = response
        .lines()
        .map(|addr| MakerAddress::new(addr.to_string()))
        .collect();

    Ok(addresses)
}

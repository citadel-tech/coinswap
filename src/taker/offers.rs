//! Download, process and store Maker offers from the directory-server.
//!
//! It defines structures like [OfferAndAddress] and [MakerAddress] for representing maker offers and addresses.
//! The [OfferBook] struct keeps track of good and bad makers, and it provides methods for managing offers.
//! The module handles the syncing of the offer book with addresses obtained from directory servers and local configurations.
//! It uses asynchronous channels for concurrent processing of maker offers.

use std::{fmt, thread, time::Duration};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::mpsc,
};

use bitcoin::Network;

use crate::{protocol::messages::Offer, utill::ConnectionType};

use crate::market::directory::DirectoryServerError;

use super::{config::TakerConfig, routines::download_maker_offer};
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

    /// Gets the list of bad makers.
    pub fn get_bad_makers(&self) -> Vec<&OfferAndAddress> {
        self.bad_makers.iter().collect()
    }
}

/// Synchronizes the offer book with specific maker addresses.
pub async fn fetch_offer_from_makers(
    maker_addresses: Vec<MakerAddress>,
    config: &TakerConfig,
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
pub async fn fetch_addresses_from_dns(
    socks_port: Option<u16>,
    directory_server_address: String,
    _network: Network,
    number_of_makers: u16,
    connection_type: ConnectionType,
) -> Result<Vec<MakerAddress>, DirectoryServerError> {
    loop {
        let result: Result<Vec<MakerAddress>, DirectoryServerError> = (async {
            let mut stream = match connection_type {
                ConnectionType::CLEARNET => TcpStream::connect(directory_server_address.as_str())
                    .await
                    .unwrap(),
                ConnectionType::TOR => Socks5Stream::connect(
                    format!("127.0.0.1:{}", socks_port.unwrap_or(19050)).as_str(),
                    directory_server_address.as_str(),
                )
                .await
                .map_err(|_e| {
                    DirectoryServerError::Other(
                        "Issue with fetching maker address from directory server",
                    )
                })?
                .into_inner(),
            };

            let request_line = "GET\n";
            stream
                .write_all(request_line.as_bytes())
                .await
                .map_err(|_e| DirectoryServerError::Other("Error sending the request"))?;

            let mut response = String::new();
            stream
                .read_to_string(&mut response)
                .await
                .map_err(|_e| DirectoryServerError::Other("Error receiving the response"))?;

            let addresses: Vec<MakerAddress> = response
                .lines()
                .map(|addr| MakerAddress::new(addr.to_string()))
                .collect();

            log::info!("Maker addresses received from DNS: {:?}", addresses);

            Ok(addresses)
        })
        .await;

        match result {
            Ok(addresses) => {
                if addresses.len() < (number_of_makers as usize) {
                    thread::sleep(Duration::from_secs(10));
                    continue;
                }
                return Ok(addresses);
            }
            Err(e) => {
                log::error!("An error occurred: {:?}", e);
                thread::sleep(Duration::from_secs(10));
                continue;
            }
        }
    }
}

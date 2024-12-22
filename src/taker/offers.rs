//! Download, process and store Maker offers from the directory-server.
//!
//! It defines structures like [OfferAndAddress] and [MakerAddress] for representing maker offers and addresses.
//! The [OfferBook] struct keeps track of good and bad makers, and it provides methods for managing offers.
//! The module handles the syncing of the offer book with addresses obtained from directory servers and local configurations.
//! It uses asynchronous channels for concurrent processing of maker offers.

use std::{
    convert::TryFrom,
    fmt,
    io::Write,
    net::TcpStream,
    sync::mpsc,
    thread::{self, Builder},
};

use serde::{Deserialize, Serialize};

#[cfg(feature = "tor")]
use socks::Socks5Stream;

use crate::{
    error::NetError,
    protocol::messages::Offer,
    utill::{read_message, send_message, ConnectionType, DnsRequest, GLOBAL_PAUSE, NET_TIMEOUT},
};

use super::{config::TakerConfig, error::TakerError, routines::download_maker_offer};

/// Represents an offer along with the corresponding maker address.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Hash)]
pub struct OfferAndAddress {
    pub offer: Offer,
    pub address: MakerAddress,
}

const _REGTEST_MAKER_ADDRESSES_PORT: &[&str] = &["6102", "16102", "26102", "36102", "46102"];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct OnionAddress {
    port: String,
    onion_addr: String,
}

/// Enum representing maker addresses.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MakerAddress(OnionAddress);

impl MakerAddress {
    pub fn new(address: &str) -> Result<Self, TakerError> {
        if let Some((onion_addr, port)) = address.split_once(':') {
            Ok(Self(OnionAddress {
                port: port.to_string(),
                onion_addr: onion_addr.to_string(),
            }))
        } else {
            Err(NetError::InvalidNetworkAddress.into())
        }
    }
}

impl fmt::Display for MakerAddress {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}:{}", self.0.onion_addr, self.0.port)
    }
}

impl TryFrom<&mut TcpStream> for MakerAddress {
    type Error = std::io::Error;
    fn try_from(value: &mut TcpStream) -> Result<Self, Self::Error> {
        let socket_addr = value.peer_addr()?;
        Ok(MakerAddress(OnionAddress {
            port: socket_addr.port().to_string(),
            onion_addr: socket_addr.ip().to_string(),
        }))
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
pub fn fetch_offer_from_makers(
    maker_addresses: Vec<MakerAddress>,
    config: &TakerConfig,
) -> Result<Vec<OfferAndAddress>, TakerError> {
    let (offers_writer, offers_reader) = mpsc::channel::<Option<OfferAndAddress>>();
    // Thread pool for all connections to fetch maker offers.
    let mut thread_pool = Vec::new();
    let maker_addresses_len = maker_addresses.len();
    for addr in maker_addresses {
        let offers_writer = offers_writer.clone();
        let taker_config: TakerConfig = config.clone();
        let thread = Builder::new()
            .name(format!("maker_offer_fecth_thread_{}", addr))
            .spawn(move || -> Result<(), TakerError> {
                let offer = download_maker_offer(addr, taker_config);
                Ok(offers_writer.send(offer)?)
            })?;

        thread_pool.push(thread);
    }
    let mut result = Vec::<OfferAndAddress>::new();
    for _ in 0..maker_addresses_len {
        if let Some(offer_addr) = offers_reader.recv()? {
            result.push(offer_addr);
        }
    }

    for thread in thread_pool {
        log::debug!(
            "Joining thread : {}",
            thread.thread().name().expect("thread names expected")
        );
        let join_result = thread.join();
        if let Ok(r) = join_result {
            log::info!("Thread closing result: {:?}", r)
        } else if let Err(e) = join_result {
            log::info!("Error in internal thread: {:?}", e);
        }
    }
    Ok(result)
}

/// Retrieves advertised maker addresses from directory servers based on the specified network.
pub fn fetch_addresses_from_dns(
    socks_port: Option<u16>,
    directory_server_address: String,
    number_of_makers: usize,
    connection_type: ConnectionType,
) -> Result<Vec<MakerAddress>, TakerError> {
    // TODO: Make the communication in serde_encoded bytes.

    loop {
        let mut stream = match connection_type {
            ConnectionType::CLEARNET => TcpStream::connect(directory_server_address.as_str())?,
            #[cfg(feature = "tor")]
            ConnectionType::TOR => {
                let socket_addrs = format!("127.0.0.1:{}", socks_port.expect("Tor port expected"));
                Socks5Stream::connect(socket_addrs, directory_server_address.as_str())?.into_inner()
            }
        };

        stream.set_read_timeout(Some(NET_TIMEOUT))?;
        stream.set_write_timeout(Some(NET_TIMEOUT))?;
        stream.set_nonblocking(false)?;
        stream.flush()?;

        // Change datatype of number of makers to u32 from usize
        let request = DnsRequest::Get {
            makers: number_of_makers as u32,
        };
        if let Err(e) = send_message(&mut stream, &request) {
            log::warn!("Failed to send request. Retrying...{}", e);
            thread::sleep(GLOBAL_PAUSE);
            continue;
        }

        // Read the response
        let response: String = match read_message(&mut stream) {
            Ok(resp) => serde_cbor::de::from_slice(&resp[..])?,
            Err(e) => {
                log::error!("Error reading DNS response: {}. Retrying...", e);
                thread::sleep(GLOBAL_PAUSE);
                continue;
            }
        };

        // Parse and validate the response
        match response
            .lines()
            .map(MakerAddress::new)
            .collect::<Result<Vec<MakerAddress>, _>>()
        {
            Ok(addresses) => {
                if addresses.len() < number_of_makers {
                    log::info!(
                        "Insufficient addresses received. Need: {}, Got: {}. Retrying...",
                        number_of_makers,
                        addresses.len()
                    );
                    thread::sleep(GLOBAL_PAUSE);
                    continue;
                } else {
                    return Ok(addresses);
                }
            }
            Err(e) => {
                log::error!("Error decoding DNS response: {:?}. Retrying...", e);
                thread::sleep(GLOBAL_PAUSE);
                continue;
            }
        }
    }
}

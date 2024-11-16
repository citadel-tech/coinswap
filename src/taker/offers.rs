//! Download, process and store Maker offers from the directory-server.
//!
//! It defines structures like [OfferAndAddress] and [MakerAddress] for representing maker offers and addresses.
//! The [OfferBook] struct keeps track of good and bad makers, and it provides methods for managing offers.
//! The module handles the syncing of the offer book with addresses obtained from directory servers and local configurations.
//! It uses asynchronous channels for concurrent processing of maker offers.

use std::{
    fmt,
    io::{Read, Write},
    net::TcpStream,
    sync::mpsc,
    thread::{self, Builder},
};

use serde::{Deserialize, Serialize};
use socks::Socks5Stream;

use crate::{
    error::NetError,
    protocol::messages::Offer,
    utill::{ConnectionType, GLOBAL_PAUSE, NET_TIMEOUT},
};

use super::{config::TakerConfig, error::TakerError, routines::download_maker_offer};

/// Pairs a maker's swap offer with their network address.
///
/// Links available offers to their source maker for routing.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Hash)]
pub struct OfferAndAddress {
    /// Swap terms offered by the maker.
    pub offer: Offer,
    /// Network address of the offering maker.
    pub address: MakerAddress,
}

const _REGTEST_MAKER_ADDRESSES_PORT: &[&str] = &["6102", "16102", "26102", "36102", "46102"];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct OnionAddress {
    port: String,
    onion_addr: String,
}

/// Network address of a maker node on the coinswap network.
///
/// Wraps an onion address for Tor-based maker communication.
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

impl From<&mut TcpStream> for MakerAddress {
    fn from(value: &mut TcpStream) -> Self {
        let socket_addr = value.peer_addr().unwrap();
        MakerAddress(OnionAddress {
            port: socket_addr.port().to_string(),
            onion_addr: socket_addr.ip().to_string(),
        })
    }
}

/// Tracks maker offers and their status during swap execution.
///
/// Maintains ephemeral lists of:
/// - All discovered makers
/// - Successfully communicating makers
/// - Failed or misbehaving makers
///
// Note: Lists are not persisted between swaps.
// TODO: Persist the offerbook in disk.
#[derive(Debug, Default)]
pub struct OfferBook {
    /// All known maker offers and addresses.
    pub(super) all_makers: Vec<OfferAndAddress>,
    /// Makers with successful communication.
    pub(super) good_makers: Vec<OfferAndAddress>,
    /// Makers that failed or misbehaved.
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

/// Updates offerbook by fetching current offers from multiple makers.
///
/// Spawns threads to concurrently:
/// - Download offers from each maker
/// - Collect successful responses
/// - Handle connection timeouts
pub fn fetch_offer_from_makers(
    maker_addresses: Vec<MakerAddress>,
    config: &TakerConfig,
) -> Vec<OfferAndAddress> {
    let (offers_writer, offers_reader) = mpsc::channel::<Option<OfferAndAddress>>();
    // Thread pool for all connections to fetch maker offers.
    let mut thread_pool = Vec::new();
    let maker_addresses_len = maker_addresses.len();
    for addr in maker_addresses {
        let offers_writer = offers_writer.clone();
        let taker_config: TakerConfig = config.clone();
        let thread = Builder::new()
            .name(format!("maker_offer_fecth_thread_{}", addr))
            .spawn(move || {
                let offer = download_maker_offer(addr, taker_config);
                offers_writer.send(offer).unwrap();
            })
            .unwrap();

        thread_pool.push(thread);
    }
    let mut result = Vec::<OfferAndAddress>::new();
    for _ in 0..maker_addresses_len {
        // TODO: Remove all unwraps and return TakerError.
        if let Some(offer_addr) = offers_reader.recv().unwrap() {
            result.push(offer_addr);
        }
    }

    for thread in thread_pool {
        log::debug!(
            "Joining thread : {}",
            thread.thread().name().expect("thread names expected")
        );
        thread.join().unwrap();
    }
    result
}

/// Retrieves maker addresses from directory servers until enough are found.
///
/// Continuously queries directory servers for maker addresses:
/// - Supports both clearnet and Tor connections
/// - Retries on timeouts or insufficient addresses
/// - Sets connection timeouts for reliability
///
/// # Errors
/// - Network connection failures
/// - Invalid address format
/// - Read/write timeouts
/// - Insufficient makers available
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
            ConnectionType::TOR => {
                let socket_addrs = format!("127.0.0.1:{}", socks_port.expect("Tor port expected"));
                Socks5Stream::connect(socket_addrs, directory_server_address.as_str())?.into_inner()
            }
        };

        stream.set_read_timeout(Some(NET_TIMEOUT))?;
        stream.set_write_timeout(Some(NET_TIMEOUT))?;
        stream.flush()?;

        // TODO: Handle timeout cases like the Taker/Maker comms, with attempt count and variable delays.
        if let Err(e) = stream
            .write_all("GET\n".as_bytes())
            .and_then(|_| stream.flush())
        {
            log::error!("Error sending GET request to DNS {}.\nRe-attempting...", e);
            thread::sleep(GLOBAL_PAUSE);
            continue;
        }

        let mut response = String::new();

        if let Err(e) = stream.read_to_string(&mut response) {
            log::error!("Error reading DNS response: {}. \nRe-attempting...", e);
            thread::sleep(GLOBAL_PAUSE);
            continue;
        }

        match response
            .lines()
            .map(MakerAddress::new)
            .collect::<Result<Vec<MakerAddress>, _>>()
        {
            Ok(addresses) => {
                if addresses.len() < number_of_makers {
                    log::info!(
                        "Didn't receive enough addresses. Need: {}, Got : {}, Attempting again...",
                        number_of_makers,
                        addresses.len()
                    );
                    thread::sleep(GLOBAL_PAUSE);
                } else {
                    return Ok(addresses);
                }
            }
            Err(e) => {
                log::error!("Error decoding DNS response: {:?}. Re-attempting...", e);
                thread::sleep(GLOBAL_PAUSE);
                continue;
            }
        }
    }
}

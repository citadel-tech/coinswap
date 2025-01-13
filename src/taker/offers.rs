//! Download, process and store Maker offers from the directory-server.
//!
//! It defines structures like [OfferAndAddress] and [MakerAddress] for representing maker offers and addresses.
//! The [OfferBook] struct keeps track of good and bad makers, and it provides methods for managing offers.
//! The module handles the syncing of the offer book with addresses obtained from directory servers and local configurations.
//! It uses asynchronous channels for concurrent processing of maker offers.

use std::{
    convert::TryFrom,
    fmt,
    fs::read,
    io::{BufWriter, Write},
    net::TcpStream,
    path::Path,
    sync::mpsc,
    thread::{self, Builder},
};

use serde::{Deserialize, Serialize};

#[cfg(feature = "tor")]
use socks::Socks5Stream;

use crate::{
    error::NetError,
    protocol::messages::{DnsRequest, Offer},
    utill::{read_message, send_message, ConnectionType, GLOBAL_PAUSE, NET_TIMEOUT},
};

use super::{config::TakerConfig, error::TakerError, routines::download_maker_offer};

/// Represents an offer along with the corresponding maker address.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OfferAndAddress {
    pub(crate) offer: Offer,
    /// All maker addresses
    pub address: MakerAddress,
}

const _REGTEST_MAKER_ADDRESSES_PORT: &[&str] = &["6102", "16102", "26102", "36102", "46102"];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct OnionAddress {
    port: String,
    onion_addr: String,
}

/// Enum representing maker addresses.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MakerAddress(OnionAddress);

impl MakerAddress {
    pub(crate) fn new(address: &str) -> Result<Self, TakerError> {
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
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct OfferBook {
    pub(super) all_makers: Vec<OfferAndAddress>,
    pub(super) bad_makers: Vec<OfferAndAddress>,
}

impl OfferBook {
    // TODO: design a better offerbook:
    // - unique key.
    // - clear good-bad separations.
    // - ranking system.
    // - various categories of livelynesss, to smartly distribute try counts.

    /// Gets all "not-bad" offers.
    pub fn all_good_makers(&self) -> Vec<&OfferAndAddress> {
        self.all_makers
            .iter()
            .filter(|offer| !self.bad_makers.contains(offer))
            .collect()
    }

    /// Adds a new offer to the offer book.
    pub(crate) fn add_new_offer(&mut self, offer: &OfferAndAddress) -> bool {
        if !self.all_makers.contains(offer) {
            self.all_makers.push(offer.clone());
            true
        } else {
            false
        }
    }

    /// Adds a bad maker to the offer book.
    pub(crate) fn add_bad_maker(&mut self, bad_maker: &OfferAndAddress) -> bool {
        if !self.bad_makers.contains(bad_maker) {
            self.bad_makers.push(bad_maker.clone());
            true
        } else {
            false
        }
    }

    /// Gets the list of bad makers.
    pub(crate) fn get_bad_makers(&self) -> Vec<&OfferAndAddress> {
        self.bad_makers.iter().collect()
    }

    /// Load existing file, updates it, writes it back (errors if path doesn't exist).
    pub fn write_to_disk(&self, path: &Path) -> Result<(), TakerError> {
        let wallet_file = std::fs::OpenOptions::new().write(true).open(path)?;
        let writer = BufWriter::new(wallet_file);
        Ok(serde_cbor::to_writer(writer, &self)?)
    }

    /// Reads from a path (errors if path doesn't exist).
    pub fn read_from_disk(path: &Path) -> Result<Self, TakerError> {
        //let wallet_file = File::open(path)?;
        let mut reader = read(path)?;
        let book = match serde_cbor::from_slice::<Self>(&reader) {
            Ok(book) => book,
            Err(e) => {
                let err_string = format!("{:?}", e);
                if err_string.contains("code: TrailingData") {
                    log::info!("Offerbook has trailing data, trying to restore");
                    loop {
                        // pop the last byte and try again.
                        reader.pop();
                        match serde_cbor::from_slice::<Self>(&reader) {
                            Ok(book) => break book,
                            Err(_) => continue,
                        }
                    }
                } else {
                    return Err(e.into());
                }
            }
        };
        Ok(book)
    }
}

/// Synchronizes the offer book with specific maker addresses.
pub(crate) fn fetch_offer_from_makers(
    maker_addresses: Vec<MakerAddress>,
    config: &TakerConfig,
) -> Result<Vec<OfferAndAddress>, TakerError> {
    let (offers_writer, offers_reader) = mpsc::channel::<Option<OfferAndAddress>>();
    // Thread pool for all connections to fetch maker offers.
    let mut thread_pool = Vec::new();
    let maker_addresses_len = maker_addresses.len();
    for addr in maker_addresses {
        let offers_writer = offers_writer.clone();
        let taker_config = config.clone();
        let thread = Builder::new()
            .name(format!("maker_offer_fetch_thread_{}", addr))
            .spawn(move || -> Result<(), TakerError> {
                let offer = download_maker_offer(addr, taker_config);
                Ok(offers_writer.send(offer)?)
            })?;

        thread_pool.push(thread);
    }
    let mut result = Vec::new();
    for _ in 0..maker_addresses_len {
        if let Some(offer_addr) = offers_reader.recv()? {
            result.push(offer_addr);
        }
    }

    for thread in thread_pool {
        let join_result = thread.join();

        if let Err(e) = join_result {
            log::error!("Error while joining thread: {:?}", e);
        }
    }
    Ok(result)
}

/// Retrieves advertised maker addresses from directory servers based on the specified network.
pub fn fetch_addresses_from_dns(
    socks_port: Option<u16>,
    dns_addr: String,
    connection_type: ConnectionType,
) -> Result<Vec<MakerAddress>, TakerError> {
    if !cfg!(feature = "tor") {
        assert!(
            socks_port.is_none(),
            "Cannot use socks port without tor feature"
        );
    }

    loop {
        let mut stream = match connection_type {
            ConnectionType::CLEARNET => TcpStream::connect(dns_addr.as_str())?,
            #[cfg(feature = "tor")]
            ConnectionType::TOR => {
                let socket_addrs = format!("127.0.0.1:{}", socks_port.expect("Tor port expected"));
                Socks5Stream::connect(socket_addrs, dns_addr.as_str())?.into_inner()
            }
        };

        stream.set_read_timeout(Some(NET_TIMEOUT))?;
        stream.set_write_timeout(Some(NET_TIMEOUT))?;
        stream.set_nonblocking(false)?;
        stream.flush()?;

        if let Err(e) = send_message(&mut stream, &DnsRequest::Get) {
            log::error!("Failed to send request. Retrying...{}", e);
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
                return Ok(addresses);
            }
            Err(e) => {
                log::error!("Error decoding DNS response: {:?}. Retrying...", e);
                thread::sleep(GLOBAL_PAUSE);
                continue;
            }
        }
    }
}

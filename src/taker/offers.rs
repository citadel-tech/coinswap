//! Download, process and store Maker offers from the directory-server.
//!
//! It defines structures like [`OfferAndAddress`] and [`MakerAddress`] for representing maker offers and addresses.
//! The [`OfferBook`] struct keeps track of good and bad makers, and it provides methods for managing offers.
//! The module handles the syncing of the offer book with addresses obtained from directory servers and local configurations.
//! It uses asynchronous channels for concurrent processing of maker offers.

use std::{
    convert::TryFrom, fmt, io::BufWriter, net::TcpStream, path::Path, sync::mpsc, thread::Builder,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::protocol::messages::Offer;

use super::{config::TakerConfig, error::TakerError, routines::download_maker_offer};

/// Represents an offer along with the corresponding maker address.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OfferAndAddress {
    /// Details for Maker Offer
    pub offer: Offer,
    /// Maker Address: onion_addr:port
    pub address: MakerAddress,
    /// When this offer had been downloaded and cached locally
    pub timestamp: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct OnionAddress {
    port: String,
    onion_addr: String,
}

/// Enum representing maker addresses.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Hash)]
pub struct MakerAddress(OnionAddress);

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
/// at the start of every swap. So good and bad maker list will not be persisted.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct OfferBook {
    pub(super) all_makers: Vec<OfferAndAddress>,
    pub(super) bad_makers: Vec<OfferAndAddress>,
}

impl OfferBook {
    /// Gets all "not-bad" offers.
    pub fn all_good_makers(&self) -> Vec<&OfferAndAddress> {
        self.all_makers
            .iter()
            .filter(|offer| !self.bad_makers.contains(offer))
            .collect()
    }
    /// Gets all offers.
    pub fn all_makers(&self) -> Vec<&OfferAndAddress> {
        self.all_makers.iter().collect()
    }

    /// Adds a new offer to the offer book.
    pub fn add_new_offer(&mut self, offer: &OfferAndAddress) -> bool {
        let timestamped = OfferAndAddress {
            offer: offer.offer.clone(),
            address: offer.address.clone(),
            timestamp: Utc::now(),
        };
        if !self.all_makers.iter().any(|to| to.offer == offer.offer) {
            self.all_makers.push(timestamped);
            true
        } else {
            false
        }
    }

    /// Gets the list of addresses which were last update less than 30 minutes ago.
    /// We exclude these addresses from the next round of downloading offers.
    pub fn get_fresh_addrs(&self) -> Vec<&OfferAndAddress> {
        let now = Utc::now();
        self.all_makers
            .iter()
            .filter(|to| (now - to.timestamp).num_minutes() < 30)
            .collect()
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
        let offerdata_file = std::fs::OpenOptions::new().write(true).open(path)?;
        let writer = BufWriter::new(offerdata_file);
        Ok(serde_json::to_writer_pretty(writer, &self)?)
    }

    /// Reads from a path (errors if path doesn't exist).
    pub fn read_from_disk(path: &Path) -> Result<Self, TakerError> {
        let content = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&content)?)
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
            .name(format!("maker_offer_fetch_thread_{addr}"))
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
            log::error!("Error while joining thread: {e:?}");
        }
    }
    Ok(result)
}

impl TryFrom<String> for OnionAddress {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let mut parts = value.splitn(2, ':');
        let onion_addr = parts.next().ok_or("Missing onion address")?.to_string();
        let port = parts.next().ok_or("Missing port")?.to_string();

        if onion_addr.is_empty() || port.is_empty() {
            return Err("Empty onion address or port");
        }

        Ok(OnionAddress { onion_addr, port })
    }
}

impl TryFrom<String> for MakerAddress {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let onion = OnionAddress::try_from(value)?;
        Ok(MakerAddress(onion))
    }
}

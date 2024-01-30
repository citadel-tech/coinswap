//! Download, process and store Maker offers from the directory-server.
//!
//! It defines structures like [OfferAndAddress] and [MakerAddress] for representing maker offers and addresses.
//! The [OfferBook] struct keeps track of good and bad makers, and it provides methods for managing offers.
//! The module handles the syncing of the offer book with addresses obtained from directory servers and local configurations.
//! It uses asynchronous channels for concurrent processing of maker offers.

use std::fmt;
use std::path::PathBuf;
use std::fs::File;
use std::io::Read;

use tokio::sync::mpsc;

use bitcoin::Network;

use crate::protocol::messages::Offer;

use crate::market::directory::{
    sync_maker_addresses_from_directory_servers, DirectoryServerError
};

use super::{config::TakerConfig, routines::download_maker_offer};

/// Represents an offer along with the corresponding maker address.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Hash)]
pub struct OfferAndAddress {
    pub offer: Offer,
    pub address: MakerAddress,
}

const REGTEST_MAKER_ADDRESSES_PORT: &[&str] = &[
    "6102",
    "16102",
    "26102",
    "36102",
    "46102",
];

/// Enum representing maker addresses.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MakerAddress {
    Address(String)
}

impl MakerAddress {
    /// Returns the TCP stream address as a string.
    pub fn get_tcpstream_address(&self) -> String {
        match &self {
            MakerAddress::Address (address) => address.to_string(),
        }
    }
}

impl fmt::Display for MakerAddress {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match &self {
            MakerAddress::Address(address)=> write!(f, "{}", address),
        }
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

async fn get_regtest_maker_addresses() -> Vec<MakerAddress> {
    let onion_addr_path = PathBuf::from("/tmp/tor-rust/maker/hs-dir/hostname");
    let mut file = File::open(&onion_addr_path).unwrap();
    let mut onion_addr: String = String::new();
    file.read_to_string(&mut onion_addr).unwrap();
    onion_addr.pop(); 
    REGTEST_MAKER_ADDRESSES_PORT
        .iter()
        .map(|h| MakerAddress::Address (format!("{}:{}",onion_addr,h)))
        .collect::<Vec<MakerAddress>>()
}

/// Synchronizes the offer book with specific maker addresses.
pub async fn sync_offerbook_with_addresses(
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
    network: Network,
) -> Result<Vec<MakerAddress>, DirectoryServerError> {
    Ok(if network == Network::Regtest {
        get_regtest_maker_addresses().await
    } else {
        sync_maker_addresses_from_directory_servers(network).await?
    })
}

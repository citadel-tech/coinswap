use std::fmt;

use tokio::sync::mpsc;

use bitcoin::Network;

use crate::protocol::messages::Offer;

use crate::market::directory::{
    sync_maker_addresses_from_directory_servers, DirectoryServerError, TOR_ADDR,
};

use super::{config::TakerConfig, error::TakerError, routines::download_maker_offer};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OfferAndAddress {
    pub offer: Offer,
    pub address: MakerAddress,
}

const REGTEST_MAKER_ADDRESSES: &'static [&'static str] = &[
    "localhost:6102",
    "localhost:16102",
    "localhost:26102",
    "localhost:36102",
    "localhost:46102",
];

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MakerAddress {
    Clearnet { address: String },
    Tor { address: String },
}

impl MakerAddress {
    pub fn get_tcpstream_address(&self) -> String {
        match &self {
            MakerAddress::Clearnet { address } => address.to_string(),
            MakerAddress::Tor { address: _ } => String::from(TOR_ADDR),
        }
    }
}

impl fmt::Display for MakerAddress {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match &self {
            MakerAddress::Clearnet { address } => write!(f, "{}", address),
            MakerAddress::Tor { address } => write!(f, "{}", address),
        }
    }
}

/// An ephemeral Offerbook tracking good and bad makers. Currently, Offerbook is initiated
/// at start of every swap. So good and bad maker list will ot be persisted.
// TODO: Persist the offerbook in disk.
#[derive(Debug, Default)]
pub struct OfferBook {
    all_makers: Vec<OfferAndAddress>,
    good_makers: Vec<OfferAndAddress>,
    bad_makers: Vec<OfferAndAddress>,
}

impl OfferBook {
    pub fn get_all_untried(&self) -> Vec<&OfferAndAddress> {
        self.all_makers
            .iter()
            .filter(|offer| !self.good_makers.contains(offer) && !self.bad_makers.contains(offer))
            .collect()
    }

    pub fn add_new_offer(&mut self, offer: &OfferAndAddress) -> bool {
        if !self.all_makers.contains(offer) {
            self.all_makers.push(offer.clone());
            true
        } else {
            false
        }
    }

    pub fn add_good_maker(&mut self, good_maker: &OfferAndAddress) -> bool {
        if !self.good_makers.contains(good_maker) {
            self.good_makers.push(good_maker.clone());
            true
        } else {
            false
        }
    }

    pub fn add_bad_maker(&mut self, bad_maker: &OfferAndAddress) -> bool {
        if !self.bad_makers.contains(bad_maker) {
            self.bad_makers.push(bad_maker.clone());
            true
        } else {
            false
        }
    }

    pub async fn sync_offerbook(
        &mut self,
        network: Network,
        config: &TakerConfig,
    ) -> Result<Vec<OfferAndAddress>, TakerError> {
        let offers =
            sync_offerbook_with_addresses(get_advertised_maker_addresses(network).await?, config)
                .await;

        let new_offers = offers
            .into_iter()
            .filter(|offer| !self.bad_makers.contains(offer))
            .collect::<Vec<_>>();

        new_offers.iter().for_each(|offer| {
            self.add_new_offer(offer);
        });

        Ok(new_offers)
    }

    pub fn get_bad_makers(&self) -> Vec<&OfferAndAddress> {
        self.bad_makers.iter().collect()
    }
}

fn get_regtest_maker_addresses() -> Vec<MakerAddress> {
    REGTEST_MAKER_ADDRESSES
        .iter()
        .map(|h| MakerAddress::Clearnet {
            address: h.to_string(),
        })
        .collect::<Vec<MakerAddress>>()
}

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
            if let Err(_e) = offers_writer
                .send(download_maker_offer(addr, taker_config).await)
                .await
            {
                panic!("mpsc failed");
            }
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

pub async fn get_advertised_maker_addresses(
    network: Network,
) -> Result<Vec<MakerAddress>, DirectoryServerError> {
    Ok(if network == Network::Regtest {
        get_regtest_maker_addresses()
    } else {
        sync_maker_addresses_from_directory_servers(network).await?
    })
}

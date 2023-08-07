use std::collections::HashMap;

use bitcoin::Network;

use crate::taker::{
    offers::{get_advertised_maker_addresses, sync_offerbook_with_addresses, MakerAddress},
    TakerConfig,
};

#[tokio::main]
/// App function to download offers
pub async fn download_and_display_offers(
    _network_str: Option<String>,
    maker_address: Option<String>,
) {
    let maker_addresses = if let Some(maker_addr) = maker_address {
        vec![MakerAddress::Tor {
            address: maker_addr,
        }]
    } else {
        let network = Network::Regtest; // Default netwrok
        get_advertised_maker_addresses(network)
            .await
            .expect("unable to sync maker addresses from directory servers")
    };
    let offers_addresses =
        sync_offerbook_with_addresses(maker_addresses.clone(), &TakerConfig::default()).await;

    // TODO: This is too verbose to just print stuffs. Shorten it in a single `iter().for_each()`.
    let mut addresses_offers_map = HashMap::new();
    for offer_address in offers_addresses.iter() {
        let address_str = match &offer_address.address {
            MakerAddress::Clearnet { address } => address,
            MakerAddress::Tor { address } => address,
        };
        addresses_offers_map.insert(address_str, offer_address);
    }

    println!(
        "{:<3} {:<70} {:<12} {:<12} {:<12} {:<12} {:<12} {:<12} {:<19}",
        "n",
        "maker address",
        "max size",
        "min size",
        "abs fee",
        "amt rel fee",
        "time rel fee",
        "minlocktime",
        "fidelity bond value",
    );

    for (ii, address) in maker_addresses.iter().enumerate() {
        let address_str = match &address {
            MakerAddress::Clearnet { address } => address,
            MakerAddress::Tor { address } => address,
        };
        if let Some(offer_address) = addresses_offers_map.get(&address_str) {
            let o = &offer_address.offer;

            println!(
                "{:<3} {:<70} {:<12} {:<12} {:<12} {:<12} {:<12} {:<12}",
                ii,
                address_str,
                o.max_size,
                o.min_size,
                o.absolute_fee_sat,
                o.amount_relative_fee_ppb,
                o.time_relative_fee_ppb,
                o.minimum_locktime,
            );
        } else {
            println!("{:<3} {:<70} UNREACHABLE", ii, address_str);
        }
    }
}

#![cfg(feature = "integration-test")]
//! This test demonstrates the scenario of low swap liquidity, that is the maker doesn't have enough liqudity
//! to attempt a swap, the liquidity check will montior and report this and will log to add more
//! funds to the maker wallet and halt the main thread leading to the maker to not accept any swap offer,
//! untill it's funded enough.
//! This leads to `NotEnoughMakersInOfferBook` error during Taker's offerbook sync.
//! Later we fund the maker, and check if it's out of that liquidity check loop and can proceed with a swap.

use bitcoin::Amount;
use coinswap::{
    maker::{start_maker_server_taproot, TaprootMaker, TaprootMakerBehavior as MakerBehavior},
    taker::{
        api2::{SwapParams, TakerBehavior},
        error::TakerError,
    },
};
mod test_framework;
use test_framework::*;

use log::{info, warn};
use std::{sync::Arc, thread};

#[test]
fn test_low_swap_liquidity() {
    // ---- Setup ----
    warn!("🧪 Running Test: Low Swap Liquidity check");
    //Create a maker with normal behaviour
    let makers_config_map = vec![(7102, Some(19071), MakerBehavior::Normal)];
    //Create a Taker
    let taker_behavior = vec![TakerBehavior::Normal];
    // Initialize test framework
    let (test_framework, mut taproot_taker, taproot_maker) =
        TestFramework::init_taproot(makers_config_map, taker_behavior);

    let bitcoind = &test_framework.bitcoind;
    let taproot_taker = taproot_taker.get_mut(0).unwrap();
    let maker = &taproot_maker[0];

    info!("💰 Funding taker and maker");
    // Fund the Taproot Taker with 3 UTXOs of 0.05 BTC each
    fund_taproot_taker(taproot_taker, bitcoind, 3, Amount::from_btc(0.05).unwrap());

    // Fund the Taproot Maker with 4 UTXOs of 0.05 BTC each
    fund_taproot_makers(&taproot_maker, bitcoind, 4, Amount::from_btc(0.05).unwrap());

    // Start the Taproot Maker Server thread
    info!("🚀 Initiating Maker server...");
    let taproot_maker_threads = {
        let maker_clone = maker.clone();
        thread::spawn(move || {
            start_maker_server_taproot(maker_clone).unwrap();
        })
    };

    test_framework.register_maker_threads(vec![taproot_maker_threads]);

    let log_path = format!("{}/taker/debug.log", test_framework.temp_dir.display());
    // Wait for fidelity bond to be created, before draining the wallet.
    test_framework.assert_log(" Successfully created fidelity bond", &log_path);
    // Drain the maker wallet
    drain_maker_liquidity_after_fidelity(maker, bitcoind);
    test_framework.assert_log(
        " Low taproot swap liquidity | Min: 10000 sats | Available: 0 sats | Add Funds to: ",
        &log_path,
    );
    info!("✅ Maker Halted due to low swap liquidity as expected");

    info!("🔄 Initiating taproot coinswap (Will fail due to maker not accepting any offer due to low swap liquidity)");
    // Swap params
    let swap_params = SwapParams {
        send_amount: Amount::from_sat(500000), // 0.005 BTC
        maker_count: 1,
        tx_count: 2,
        required_confirms: 1,
        manually_selected_outpoints: None,
    };

    // Attempt the swap - it will fail
    let err = taproot_taker
        .do_coinswap(swap_params.clone())
        .expect_err("Swap should have failed due to NotEnoughMakersInOfferBook");
    assert!(
        matches!(err, TakerError::NotEnoughMakersInOfferBook),
        "Expected NotEnoughMakersInOfferBook, got: {:?}",
        err
    );
    info!("✅ Taproot coinswap failed as expected: {err:?}");

    log::info!("Adding sufficient funds to maker, to perform a swap and avoid low swap liquidity ");
    fund_taproot_makers(&taproot_maker, bitcoind, 4, Amount::from_btc(0.05).unwrap());

    // Attempt the swap again, it should succeed
    match taproot_taker.do_coinswap(swap_params.clone()) {
        Ok(Some(_report)) => {
            log::info!("✅ Taproot coinswap completed successfully!");
        }
        Ok(None) => {
            panic!("Taproot coinswap completed but no report generated (recovery occurred)");
        }
        Err(e) => {
            log::error!("Taproot coinswap failed: {:?}", e);
            panic!("Taproot coinswap failed: {:?}", e);
        }
    }

    info!("✅ Low Swap liquidity test passed");
}

fn drain_maker_liquidity_after_fidelity(maker: &Arc<TaprootMaker>, bitcoind: &bitcoind::BitcoinD) {
    use bitcoin::{
        key::CompressedPublicKey,
        secp256k1::{rand::rngs::OsRng, Secp256k1, SecretKey},
        Address, Network, PublicKey,
    };
    use bitcoind::bitcoincore_rpc::RpcApi;
    use coinswap::utill::MIN_FEE_RATE;
    use std::convert::TryFrom;

    let secp = Secp256k1::new();
    let pubkey = SecretKey::new(&mut OsRng).public_key(&secp);
    let addr = Address::p2wpkh(
        &CompressedPublicKey::try_from(PublicKey::new(pubkey)).unwrap(),
        Network::Regtest,
    );
    let coins = maker
        .wallet()
        .read()
        .unwrap()
        .list_descriptor_utxo_spend_info();
    let mut wallet = maker.wallet().write().unwrap();
    let tx = wallet
        .spend_from_wallet(
            MIN_FEE_RATE,
            coinswap::wallet::Destination::Sweep(addr),
            &coins,
        )
        .unwrap();
    bitcoind.client.send_raw_transaction(&tx).unwrap();
}

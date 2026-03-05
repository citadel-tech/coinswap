#![cfg(feature = "integration-test")]
//! This test demonstrates the scenario when the maker violates the accepted fidelity_timelock limit,
//! During offerbook sync, the taker discovers this during offerbook sync and rejects maker's offer
//! leading to `NotEnoughMakersInOfferBook` error.
//! Later We restart the maker with faulty config that is setting the fidelity_timelock to an
//! unacceptable block count, the Maker thus results an error saying "Invalid fidelity timelock"

use bitcoin::Amount;
use coinswap::{
    maker::{
        start_maker_server_taproot, MakerError, TaprootMaker, TaprootMakerBehavior as MakerBehavior,
    },
    taker::{
        api2::{SwapParams, TakerBehavior},
        error::TakerError,
    },
    wallet::WalletError,
};
mod test_framework;
use test_framework::*;

use log::{info, warn};
use std::{fs, sync::atomic::Ordering::Relaxed, thread, time::Duration};

#[test]
fn fidelity_limit_violation() {
    // ---- Setup ----
    warn!("🧪 Running Test: Fidelity Timelock violation");
    //Create a maker with normal behaviour
    let makers_config_map = vec![(7102, Some(19071), MakerBehavior::InvalidFidelityTimelock)];
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

    // Wait for taproot maker to complete setup
    while !maker.is_setup_complete.load(Relaxed) {
        info!("⏳ Waiting for taproot maker setup completion");
        thread::sleep(Duration::from_secs(10));
    }

    // Sync wallets after setup
    maker.wallet().write().unwrap().sync_and_save().unwrap();

    // Get balances before swap
    verify_maker_pre_swap_balance_taproot(&taproot_maker);
    info!("🔄 Initiating taproot coinswap (Will fail due to invalid fidelity timelock)");

    // Swap params - small amount for faster testing
    let swap_params = SwapParams {
        send_amount: Amount::from_sat(500000), // 0.005 BTC
        maker_count: 1,
        tx_count: 2,
        required_confirms: 1,
        manually_selected_outpoints: None,
    };

    // Attempt the swap - it will fail
    let err = taproot_taker
        .do_coinswap(swap_params)
        .expect_err("Swap should have failed due to NotEnoughMakersInOfferBook");
    assert!(
        matches!(err, TakerError::NotEnoughMakersInOfferBook),
        "Expected NotEnoughMakersInOfferBook, got: {:?}",
        err
    );
    info!("✅ Taproot coinswap failed as expected: {err:?}");

    info!("🛑 Shutting down maker to simulate restart with corrupted config");
    drop(test_framework);

    // Change maker config fidelity_timelock
    let config_path = maker.data_dir().join("config.toml");
    let mut contents = fs::read_to_string(&config_path).unwrap();
    contents = contents.replace("fidelity_timelock = 6000", "fidelity_timelock = 100");
    fs::write(&config_path, contents).unwrap();
    let port_zmq = 28332 + bip39::rand::random::<u16>() % 1000;
    let zmq_addr = format!("tcp://127.0.0.1:{port_zmq}");

    // Attempt restart
    info!("🔁 Restarting maker with non-acceptable fidelity_timelock");
    let restart_result = TaprootMaker::init(
        Some(maker.data_dir().to_path_buf()),
        Some("maker_test".to_string()),
        None,
        None,
        None,
        None,
        None,
        None,
        zmq_addr.clone(),
        None,
        None,
    );

    assert!(matches!(
        restart_result,
        Err(MakerError::Wallet(WalletError::Fidelity(_)))
    ));
    info!("✅ Maker doesn't started as expected",);

    info!("✅ Fidelity Timelock violation passed");
}

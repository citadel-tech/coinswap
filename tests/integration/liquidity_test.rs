//! This test demonstrates the scenario of low swap liquidity, that is the Maker doesn't have enough
//! liquidity to attempt a swap. The liquidity check will monitor and report this and will log to
//! add more funds to the Maker wallet and halt the main thread leading to the Maker to not accept
//! any swap offer, until it's funded enough.
//! This leads to `NotEnoughMakersInOfferBook` error during Taker's offerbook sync.
//! Later we fund the maker, and check if it's out of that liquidity check loop and can proceed
//! with a swap.

use bitcoin::Amount;
use coinswap::{
    maker::{start_server, MakerServer},
    protocol::common_messages::ProtocolVersion,
    taker::{SwapParams, TakerBehavior},
    utill::MIN_FEE_RATE,
    wallet::{AddressType, Destination},
};

use super::test_framework::*;

use log::{info, warn};
use std::sync::{atomic::Ordering::Relaxed, Arc};

#[test]
fn test_low_swap_liquidity() {
    // ---- Setup ----
    warn!("Running Test: Low Swap Liquidity check");

    // Create a maker with normal behaviour
    let makers_config_map = vec![(8402, None)];
    let taker_behavior = vec![TakerBehavior::Normal];

    // Initialize test framework
    let (test_framework, mut takers, makers, block_generation_handle) =
        TestFramework::init(makers_config_map, taker_behavior, vec![]);

    let bitcoind = &test_framework.bitcoind;
    let taker = takers.get_mut(0).unwrap();
    let maker = &makers[0];

    info!("Funding taker and maker");
    // Fund the taker with 3 UTXOs of 0.05 BTC each (Taproot)
    fund_taker(
        taker,
        bitcoind,
        3,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2TR,
    );

    // Fund the Maker with 4 UTXOs of 0.05 BTC each (Taproot)
    fund_makers(
        &makers,
        bitcoind,
        4,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2TR,
    );

    // Start the Maker Server thread
    info!("Initiating Maker server...");
    let maker_thread = {
        let maker_clone = maker.clone();
        std::thread::spawn(move || {
            start_server(maker_clone).unwrap();
        })
    };

    // Wait for maker to complete setup (including fidelity bond creation)
    wait_for_makers_setup(std::slice::from_ref(maker), 120);

    // Drain the Maker wallet after fidelity bond is created
    drain_maker_liquidity_after_fidelity(maker, bitcoind);
    // Mine a block to confirm the drain, then sync maker wallet
    generate_blocks(bitcoind, 1);
    maker.wallet.write().unwrap().sync_and_save().unwrap();

    info!("Maker should be halted due to low swap liquidity");

    info!("Initiating coinswap (Will fail due to maker not accepting any offer due to low swap liquidity)");

    // Swap params
    let swap_params = SwapParams::new(ProtocolVersion::Taproot, Amount::from_sat(500000), 1)
        .with_tx_count(2)
        .with_required_confirms(1);

    // Attempt the swap - it will fail because maker has no liquidity
    let err = taker
        .prepare_coinswap(swap_params.clone())
        .expect_err("Swap should have failed due to insufficient maker liquidity");
    info!("Coinswap failed as expected: {err:?}");

    log::info!("Adding sufficient funds to maker to perform a swap and avoid low swap liquidity");
    fund_makers(
        &makers,
        bitcoind,
        4,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2TR,
    );

    // Attempt the swap again, it should succeed
    let swap_params = SwapParams::new(ProtocolVersion::Taproot, Amount::from_sat(500000), 1)
        .with_tx_count(2)
        .with_required_confirms(1);

    let summary = taker
        .prepare_coinswap(swap_params)
        .expect("prepare_coinswap should succeed after funding");

    match taker.start_coinswap(&summary.swap_id) {
        Ok(_report) => {
            log::info!("Coinswap completed successfully after re-funding!");
        }
        Err(e) => {
            log::error!("Coinswap failed: {:?}", e);
            panic!("Coinswap failed: {:?}", e);
        }
    }

    maker.shutdown.store(true, Relaxed);
    maker_thread.join().unwrap();
    test_framework.stop();
    block_generation_handle.join().unwrap();

    info!("Low Swap liquidity test passed");
}

fn drain_maker_liquidity_after_fidelity(maker: &Arc<MakerServer>, bitcoind: &bitcoind::BitcoinD) {
    use bitcoin::{
        secp256k1::{rand::rngs::OsRng, Secp256k1, SecretKey},
        Address, Network,
    };
    use bitcoind::bitcoincore_rpc::RpcApi;

    let secp = Secp256k1::new();
    let keypair = bitcoin::key::Keypair::from_secret_key(&secp, &SecretKey::new(&mut OsRng));
    let (xonly, _) = keypair.x_only_public_key();
    let addr = Address::p2tr(&secp, xonly, None, Network::Regtest);
    let coins = maker
        .wallet
        .read()
        .unwrap()
        .list_descriptor_utxo_spend_info();
    let mut wallet = maker.wallet.write().unwrap();
    let tx = wallet
        .spend_from_wallet(MIN_FEE_RATE, Destination::Sweep(addr), &coins)
        .unwrap();
    bitcoind.client.send_raw_transaction(&tx).unwrap();
}

#![cfg(feature = "integration-test")]
use bitcoin::Amount;
use coinswap::{
    maker::{start_maker_server, MakerBehavior},
    taker::{SwapParams, TakerBehavior},
    utill::{ConnectionType, DEFAULT_TX_FEE_RATE},
    wallet::Destination,
};
use std::sync::Arc;

use bitcoind::bitcoincore_rpc::RpcApi;

mod test_framework;
use test_framework::*;

use log::{info, warn};
use std::{assert_eq, sync::atomic::Ordering::Relaxed, thread, time::Duration};

/// This demostrates a concurrent coinswap round between a Taker and 4 Makers (2 braches). Nothing
/// goes wrong and the coinswap completes successfully.
#[test]
fn test_concurrent_coinswap() {
    // ---- Setup ----

    // 4 Makers with Normal behavior.
    let makers_config_map = [
        ((6102, Some(19051)), MakerBehavior::Normal),
        ((16102, Some(19052)), MakerBehavior::Normal),
        ((26102, Some(19053)), MakerBehavior::Normal),
        ((36102, Some(19054)), MakerBehavior::Normal),
    ];

    let connection_type = ConnectionType::CLEARNET;

    // Initiate test framework, Makers and a Taker with default behavior.
    let (test_framework, mut taker, makers, directory_server_instance, block_generation_handle) =
        TestFramework::init(
            makers_config_map.into(),
            TakerBehavior::Normal,
            connection_type,
        );

    warn!("Running Test: Standard Coinswap Procedure");
    let bitcoind = &test_framework.bitcoind;

    // Fund the Taker  with 3 utxos of 0.05 btc each and do basic checks on the balance
    let org_taker_spend_balance =
        fund_and_verify_taker(&mut taker, bitcoind, 3, Amount::from_btc(0.05).unwrap());

    // Fund the Maker with 4 utxos of 0.05 btc each and do basic checks on the balance.
    let makers_ref = makers.iter().map(Arc::as_ref).collect::<Vec<_>>();
    fund_and_verify_maker(makers_ref, bitcoind, 4, Amount::from_btc(0.05).unwrap());

    //  Start the Maker Server threads
    log::info!("Initiating Maker...");

    let maker_threads = makers
        .iter()
        .map(|maker| {
            let maker_clone = maker.clone();
            thread::spawn(move || {
                start_maker_server(maker_clone).unwrap();
            })
        })
        .collect::<Vec<_>>();

    // Makers take time to fully setup.
    let org_maker_spend_balances = makers
        .iter()
        .map(|maker| {
            while !maker.is_setup_complete.load(Relaxed) {
                log::info!("Waiting for maker setup completion");
                // Introduce a delay of 10 seconds to prevent write lock starvation.
                thread::sleep(Duration::from_secs(10));
                continue;
            }

            // Check balance after setting up maker server.
            let wallet = maker.wallet.read().unwrap();
            let all_utxos = wallet.get_all_utxo().unwrap();

            let balances = wallet.get_balances(Some(&all_utxos)).unwrap();

            assert_eq!(balances.regular, Amount::from_btc(0.14999).unwrap());
            assert_eq!(balances.fidelity, Amount::from_btc(0.05).unwrap());
            assert_eq!(balances.swap, Amount::ZERO);
            assert_eq!(balances.contract, Amount::ZERO);

            balances.spendable
        })
        .collect::<Vec<_>>();

    // Initiate Coinswap
    log::info!("Initiating coinswap protocol");

    // Swap params for coinswap.
    let swap_params = SwapParams {
        send_amount: Amount::from_sat(500000),
        maker_count: 2,
        tx_count: 3,
        required_confirms: 1,
        branches: 2,
    };

    taker.do_coinswap(swap_params).unwrap();

    // After Swap is done,  wait for maker threads to conclude.
    makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    log::info!("All coinswaps processed successfully. Transaction complete.");

    // Shutdown Directory Server
    directory_server_instance.shutdown.store(true, Relaxed);

    thread::sleep(Duration::from_secs(10));

    //-------- Fee Tracking and Workflow:------------
    //
    // Branch 1
    //
    // | Participant    | Amount Received (Sats) | Amount Forwarded (Sats) | Fee (Sats) | Funding Mining Fees (Sats) | Total Fees (Sats) |
    // |----------------|------------------------|-------------------------|------------|----------------------------|-------------------|
    // | **Taker**      | _                      | 250,000                 | _          | 3,000                      | 3,000             |
    // | **Maker16102** | 250,000                | 229,750                 | 17,250     | 3,000                      | 20,250            |
    // | **Maker6102**  | 229,750                | 215,411                 | 11,339     | 3,000                      | 14,339            |
    // Branch 2
    //
    // | Participant    | Amount Received (Sats) | Amount Forwarded (Sats) | Fee (Sats) | Funding Mining Fees (Sats) | Total Fees (Sats) |
    // |----------------|------------------------|-------------------------|------------|----------------------------|-------------------|
    // | **Taker**      | _                      | 250,000                 | _          | 3,000                      | 3,000             |
    // | **Maker36102** | 250,000                | 229,750                 | 17,250     | 3,000                      | 20,250            |
    // | **Maker26102** | 229,750                | 215,411                 | 11,339     | 3,000                      | 14,339            |
    //
    // ## 3. Final Outcome for Taker (Successful Coinswap):
    //
    // | Participant   | Coinswap Outcome (Sats)                                                     |
    // |---------------|-----------------------------------------------------------------------------|
    // | **Taker**     | 215,411 = 250,000 - (Total Fees for Maker16102 + Total Fees for Maker6102)  |
    // | **Taker**     | 215,411 = 250,000 - (Total Fees for Maker36102 + Total Fees for Maker26102) |
    // |   Total       | 430,822                                                                     |
    //

    info!("Balance check successful.");

    // Check spending from swapcoins.
    info!("Checking Spend from Swapcoin");

    let taker_wallet_mut = taker.get_wallet_mut();
    let swap_coins = taker_wallet_mut
        .list_incoming_swap_coin_utxo_spend_info(None)
        .unwrap();

    let addr = taker_wallet_mut.get_next_internal_addresses(1).unwrap()[0].to_owned();

    let tx = taker_wallet_mut
        .spend_from_wallet(DEFAULT_TX_FEE_RATE, Destination::Sweep(addr), &swap_coins)
        .unwrap();

    assert_eq!(
        tx.input.len(),
        6,
        "Not all swap coin utxos got included in the spend transaction"
    );

    bitcoind.client.send_raw_transaction(&tx).unwrap();
    generate_blocks(bitcoind, 1);

    let balances = taker_wallet_mut.get_balances(None).unwrap();

    assert_eq!(balances.swap, Amount::ZERO);
    assert_eq!(balances.regular, Amount::from_btc(0.14937206).unwrap());

    info!("All checks successful. Terminating integration test case");

    test_framework.stop();
    block_generation_handle.join().unwrap();
}

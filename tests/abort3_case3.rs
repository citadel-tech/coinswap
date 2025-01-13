#![cfg(feature = "integration-test")]
use bitcoin::Amount;
use coinswap::{
    maker::{start_maker_server, MakerBehavior},
    taker::{SwapParams, TakerBehavior},
    utill::ConnectionType,
};
use std::sync::Arc;

mod test_framework;
use test_framework::*;

use log::{info, warn};
use std::{sync::atomic::Ordering::Relaxed, thread, time::Duration};

/// ABORT 3: Maker Drops After Setup
/// Case 3: CloseAtHashPreimage
///
/// Maker closes connection at hash preimage handling. Funding txs are already broadcasted.
/// The Maker will loose contract txs fees in that case, so it's not a malice.
/// Taker waits for the response until timeout. Aborts if the Maker doesn't show up.
#[test]
fn abort3_case3_close_at_hash_preimage_handover() {
    // ---- Setup ----

    // 6102 is naughty. And theres not enough makers.
    let makers_config_map = [
        ((6102, None), MakerBehavior::CloseAtHashPreimage),
        ((16102, None), MakerBehavior::Normal),
    ];

    // Initiate test framework, Makers.
    // Taker has normal behavior.
    let (test_framework, mut taker, makers, directory_server_instance, block_generation_handle) =
        TestFramework::init(
            makers_config_map.into(),
            TakerBehavior::Normal,
            ConnectionType::CLEARNET,
        );

    warn!("Running Test: Maker closes conneciton at hash preimage handling");

    // Fund the Taker  with 3 utxos of 0.05 btc each and do basic checks on the balance
    let org_taker_spend_balance = fund_and_verify_taker(
        &mut taker,
        &test_framework.bitcoind,
        3,
        Amount::from_btc(0.05).unwrap(),
    );

    // Fund the Maker with 4 utxos of 0.05 btc each and do basic checks on the balance.
    let makers_ref = makers.iter().map(Arc::as_ref).collect::<Vec<_>>();
    fund_and_verify_maker(
        makers_ref,
        &test_framework.bitcoind,
        4,
        Amount::from_btc(0.05).unwrap(),
    );

    //  Start the Maker Server threads
    info!("Initiating Maker...");

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
                info!("Waiting for maker setup completion");
                // Introduce a delay of 10 seconds to prevent write lock starvation.
                thread::sleep(Duration::from_secs(10));
                continue;
            }

            // Check balance after setting up maker server.
            let wallet = maker.wallet.read().unwrap();
            let all_utxos = wallet.get_all_utxo().unwrap();

            let seed_balance = wallet.balance_descriptor_utxo(Some(&all_utxos)).unwrap();

            let fidelity_balance = wallet.balance_fidelity_bonds(Some(&all_utxos)).unwrap();

            let swapcoin_balance = wallet.balance_swap_coins(Some(&all_utxos)).unwrap();

            let live_contract_balance = wallet.balance_live_contract(Some(&all_utxos)).unwrap();

            assert_eq!(seed_balance, Amount::from_btc(0.14999).unwrap());
            assert_eq!(fidelity_balance, Amount::from_btc(0.05).unwrap());
            assert_eq!(swapcoin_balance, Amount::ZERO);
            assert_eq!(live_contract_balance, Amount::ZERO);

            seed_balance + swapcoin_balance
        })
        .collect::<Vec<_>>();

    // Initiate Coinswap
    info!("Initiating coinswap protocol");

    // Swap params for coinswap.
    let swap_params = SwapParams {
        send_amount: Amount::from_sat(500000),
        maker_count: 2,
        tx_count: 3,
        required_confirms: 1,
    };
    taker.do_coinswap(swap_params).unwrap();

    // After Swap is done,  wait for maker threads to conclude.
    makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    //TODO: Start the faulty maker again, and validate its recovery.

    info!("All coinswaps processed successfully. Transaction complete.");

    // Shutdown Directory Server
    directory_server_instance.shutdown.store(true, Relaxed);

    thread::sleep(Duration::from_secs(10));

    //-------- Fee Tracking and Workflow:--------------------------------------------------------------------------
    //
    // This fee scenario would occur in both cases whether Maker6102 is the first or last maker.

    // Case 1: Maker6102 is the first maker
    // Workflow: Taker -> Maker6102(CloseAtHashPreimage) -> Maker16102
    //
    //
    // | Participant    | Amount Received (Sats) | Amount Forwarded (Sats) | Fee (Sats) | Funding Mining Fees (Sats) | Total Fees (Sats) |
    // |----------------|------------------------|-------------------------|------------|----------------------------|-------------------|
    // | **Taker**      | _                      | 500,000                 | _          | 3,000                      | 3,000             |
    // | **Maker16102** | 500,000                | 463,500                 | 33,500     | 3,000                      | 36,500            |
    // | **Maker6102**  | 463,500                | 438,642                 | 21,858     | 3,000                      | 24,858            |
    //
    //  Maker6102 => DropConnectionAfterFullSetup
    //
    // Participants regain their initial funding amounts but incur a total loss of **6,768 sats**
    // due to mining fees (recovery + initial transaction fees).
    //
    // | Participant    | Mining Fee for Contract txes (Sats) | Timelock Fee (Sats) | Funding Fee (Sats) | Total Recovery Fees (Sats) |
    // |----------------|------------------------------------|---------------------|--------------------|----------------------------|
    // | **Taker**      | 3,000                              | 768                 | 3,000             | 6,768                      |
    // | **Maker16102** | 3,000                              | 768                 | 3,000             | 6,768                      |
    // | **Maker6102**  | 3,000                              | 768                 | 3,000             | 6,768                      |
    //
    // Case 2: Maker16102 is the last maker.
    // Workflow: Taker -> Maker16102 -> Maker16102(CloseAtHashPreimage)
    //
    // Same as Case 1.
    //-----------------------------------------------------------------------------------------------------------------------------------------------

    // After Swap checks:
    verify_swap_results(
        &taker,
        &makers,
        org_taker_spend_balance,
        org_maker_spend_balances,
    );

    info!("All checks successful. Terminating integration test case");

    test_framework.stop();
    block_generation_handle.join().unwrap();
}

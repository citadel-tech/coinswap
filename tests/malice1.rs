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
use std::{assert_eq, sync::atomic::Ordering::Relaxed, thread, time::Duration};

/// Malice 1: Taker Broadcasts contract transactions prematurely.
///
/// The Makers identify the situation and get their money back via contract txs. This is
/// a potential DOS on Makers. But Taker would lose money too for doing this.
#[test]
fn malice1_taker_broadcast_contract_prematurely() {
    // ---- Setup ----

    let makers_config_map = [
        ((6102, None), MakerBehavior::Normal),
        ((16102, None), MakerBehavior::Normal),
    ];

    let taker_behavior = vec![TakerBehavior::BroadcastContractAfterFullSetup];
    // Initiate test framework, Makers.
    // Taker has normal behavior.
    let (test_framework, mut takers, makers, directory_server_instance, block_generation_handle) =
        TestFramework::init(
            makers_config_map.into(),
            taker_behavior,
            ConnectionType::CLEARNET,
        );

    warn!("🧪 Running Test: Malice 1 - Taker broadcasts contract transaction prematurely");

    info!("💰 Funding taker and makers");
    // Fund the Taker  with 3 utxos of 0.05 btc each and do basic checks on the balance
    let taker = &mut takers[0];
    let org_taker_spend_balance = fund_and_verify_taker(
        taker,
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
    info!("🚀 Initiating Maker servers");

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
                info!("⏳ Waiting for maker setup completion");
                // Introduce a delay of 10 seconds to prevent write lock starvation.
                thread::sleep(Duration::from_secs(10));
                continue;
            }

            // Check balance after setting up maker server.
            let wallet = maker.wallet.read().unwrap();

            let balances = wallet.get_balances().unwrap();

            assert_eq!(balances.regular, Amount::from_btc(0.14999).unwrap());
            assert_eq!(balances.fidelity, Amount::from_btc(0.05).unwrap());
            assert_eq!(balances.swap, Amount::ZERO);
            assert_eq!(balances.contract, Amount::ZERO);

            balances.spendable
        })
        .collect::<Vec<_>>();

    // Initiate Coinswap
    info!("🔄 Initiating coinswap protocol");

    // Swap params for coinswap.
    let swap_params = SwapParams {
        send_amount: Amount::from_sat(500000),
        maker_count: 2,
        tx_count: 3,
    };
    taker.do_coinswap(swap_params).unwrap();

    // After Swap is done, wait for maker threads to conclude.
    makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    info!("🎯 All coinswaps processed successfully. Transaction complete.");

    // Shutdown Directory Server
    directory_server_instance.shutdown.store(true, Relaxed);

    thread::sleep(Duration::from_secs(10));

    ///////////////////
    let taker_wallet = taker.get_wallet_mut();
    taker_wallet.sync().unwrap();

    // Synchronize each maker's wallet.
    for maker in makers.iter() {
        let mut wallet = maker.get_wallet().write().unwrap();
        wallet.sync().unwrap();
    }
    ///////////////

    //-------- Fee Tracking and Workflow:------------
    //
    // | Participant    | Amount Received (Sats) | Amount Forwarded (Sats) | Fee (Sats) | Funding Mining Fees (Sats) | Total Fees (Sats) |
    // |----------------|------------------------|-------------------------|------------|----------------------------|-------------------|
    // | **Taker**      | _                      | 500,000                 | _          | 3,000                      | 3,000             |
    // | **Maker16102** | 500,000                | 463,500                 | 33,500     | 3,000                      | 36,500            |
    // | **Maker6102**  | 463,500                | 438,642                 | 21,858     | 3,000                      | 24,858            |
    //
    //  **Taker** => BroadcastContractAfterFullSetup
    //
    // Participants regain their initial funding amounts but incur a total loss of **6,768 sats**
    // due to mining fees (recovery + initial transaction fees).
    //
    // | Participant    | Mining Fee for Contract txes (Sats) | Timelock Fee (Sats) | Funding Fee (Sats) | Total Recovery Fees (Sats) |
    // |----------------|------------------------------------|---------------------|--------------------|----------------------------|
    // | **Taker**      | 3,000                              | 768                 | 3,000             | 6,768                      |
    // | **Maker16102** | 3,000                              | 768                 | 3,000             | 6,768                      |
    // | **Maker6102**  | 3,000                              | 768                 | 3,000             | 6,768                      |

    info!("📊 Verifying malicious taker scenario results");
    // After Swap checks:
    verify_swap_results(
        taker,
        &makers,
        org_taker_spend_balance,
        org_maker_spend_balances,
    );

    info!("🎉 All checks successful. Terminating integration test case");

    test_framework.stop();
    block_generation_handle.join().unwrap();
}

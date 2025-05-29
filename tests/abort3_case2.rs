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
/// Case 2: CloseAtContractSigsForRecvr
///
/// Maker closes the connection after sending a `ContractSigsForRecvr`. Funding txs are already broadcasted.
/// The Maker will lose contract txs fees in that case, so it's not malice.
/// Taker waits for the response until timeout. Aborts if the Maker doesn't show up.
#[test]
fn abort3_case2_close_at_contract_sigs_for_recvr() {
    // ---- Setup ----

    // 6102 is naughty. And theres not enough makers.
    let naughty = 6102;
    let makers_config_map = [
        ((naughty, None), MakerBehavior::CloseAtContractSigsForRecvr),
        ((16102, None), MakerBehavior::Normal),
    ];

    let taker_behavior = vec![TakerBehavior::Normal];
    // Initiate test framework, Makers.
    // Taker has normal behavior.
    let (test_framework, mut takers, makers, directory_server_instance, block_generation_handle) =
        TestFramework::init(
            makers_config_map.into(),
            taker_behavior,
            ConnectionType::CLEARNET,
        );

    warn!("🧪 Running Test: Maker closes connection after sending a ContractSigsForRecvr");

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

    // -------- Fee Tracking and Workflow --------
    //
    // Case 1: Maker6102 is the First Maker.
    // Workflow: Taker -> Maker6102 (CloseAtContractSigsForRecvr) -----> Maker16102
    //
    // | Participant    | Amount Received (Sats) | Amount Forwarded (Sats) | Fee (Sats) | Funding Mining Fees (Sats) | Total Fees (Sats) |
    // |----------------|------------------------|-------------------------|------------|----------------------------|-------------------|
    // | **Taker**      | _                      | 500,000                 | _          | 3,000                      | 3,000             |
    // | **Maker6102**  | 500,000                | 463,500                 | 33,500     | 3,000                      | 36,500            |
    //
    // - Taker sends [ProofOfFunding] of Maker6102 to Maker16102, who replies with [ReqContractSigsForRecvrAndSender].
    // - Taker forwards [ReqContractSigsForRecvr] to Maker6102, but Maker6102 doesn't respond.
    // - After a timeout, both Taker and Maker6102 recover from the swap, incurring losses.
    //
    // Final Outcome for Taker & Maker6102 (Recover from Swap):
    //
    // | Participant                                         | Mining Fee for Contract txes (Sats) | Timelock Fee (Sats) | Funding Fee (Sats) | Total Recovery Fees (Sats) |
    // |-----------------------------------------------------|------------------------------------|---------------------|--------------------|----------------------------|
    // | **Taker**                                           | 3,000                              | 768                 | 3,000              | 6,768                      |
    // | **Maker6102** (Marked as a bad maker by the Taker)  | 3,000                              | 768                 | 3,000              | 6,768                      |
    //
    // - Both **Taker** and **Maker6102** regain their initial funding amounts but incur a total loss of **6,768 sats** due to mining fees.
    //
    // Final Outcome for Maker16102:
    //
    // | Participant    | Coinswap Outcome (Sats)                 |
    // |----------------|------------------------------------------|
    // | **Maker16102** | 0                                        |
    //
    // ------------------------------------------------------------------------------------------------------------------------
    //
    // Case 2: Maker6102 is the Second Maker.
    // Workflow: Taker -> Maker16102 -> Maker6102 (CloseAtContractSigsForRecvr)
    //
    // In this case, the Coinswap completes successfully since Maker6102, being the last maker, does not receive [ReqContractSigsForRecvr] from the Taker.
    //
    // The Fee balance would look like `standard_swap` IT for this case.

    info!("🚫 Verifying naughty maker gets banned");
    // Maker gets banned for being naughty.
    assert_eq!(
        format!("127.0.0.1:{naughty}"),
        taker.get_bad_makers()[0].address.to_string()
    );

    info!("📊 Verifying swap results after connection close");
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

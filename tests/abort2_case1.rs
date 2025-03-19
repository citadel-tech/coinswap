#![cfg(feature = "integration-test")]
use bitcoin::Amount;
use bitcoind::bitcoincore_rpc::RpcApi;
use coinswap::{
    maker::{start_maker_server, MakerBehavior},
    taker::{SwapParams, TakerBehavior},
    utill::{ConnectionType, DEFAULT_TX_FEE_RATE},
};
use std::sync::Arc;
mod test_framework;
use coinswap::wallet::Destination;
use log::{info, warn};
use std::{sync::atomic::Ordering::Relaxed, thread, time::Duration};
use test_framework::*;

/// ABORT 2: Maker Drops Before Setup
/// This test demonstrates the situation where a Maker prematurely drops connections after doing
/// initial protocol handshake. This should not necessarily disrupt the round, the Taker will try to find
/// more makers in his address book and carry on as usual. The Taker will mark this Maker as "bad" and will
/// not swap this maker again.
///
/// CASE 1: Maker Drops Before Sending Sender's Signature, and Taker carries on with a new Maker.
#[test]
fn test_abort_case_2_move_on_with_other_makers() {
    // ---- Setup ----

    // 6102 is naughty. But theres enough good ones.
    let makers_config_map = [
        ((6102, None), MakerBehavior::Normal),
        (
            (16102, None),
            MakerBehavior::CloseAtReqContractSigsForSender,
        ),
        ((26102, None), MakerBehavior::Normal),
    ];

    // Initiate test framework, Makers.
    // Taker has normal behavior.
    let (test_framework, mut taker, makers, directory_server_instance, block_generation_handle) =
        TestFramework::init(
            makers_config_map.into(),
            TakerBehavior::Normal,
            ConnectionType::CLEARNET,
        );

    warn!(
        "Running Test: Maker 6102 closes before sending sender's sigs. Taker moves on with other Makers."
    );

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

            let balances = wallet.get_balances().unwrap();

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

    ///////////////////
    let taker_wallet = taker.get_wallet_mut();
    taker_wallet.sync().unwrap();

    // Synchronize each maker's wallet.
    for maker in makers.iter() {
        let mut wallet = maker.get_wallet().write().unwrap();
        wallet.sync().unwrap();
    }
    ///////////////

    // ----------------------Swap Completed Successfully-----------------------------------------------------------

    // +------------------------------------------------------------------------------------------------------+
    // | ## Fee Tracking and Workflow                                                                       |
    // +------------------------------------------------------------------------------------------------------+
    // |                                                                                                      |
    // | ### Assumptions:                                                                                     |
    // | 1. **Taker connects to Maker16102 as the first Maker.**                                              |
    // | 2. **Workflow:** Taker → Maker16102 (`CloseAtReqContractSigsForSender`) → Maker6102 → Maker26102 →   |
    // |    Taker.                                                                                           |
    // |                                                                                                      |
    // | ### Fee Breakdown:                                                                                   |
    // |                                                                                                      |
    // | +------------------+-------------------------+--------------------------+------------+----------------+|
    // | | Participant      | Amount Received (Sats) | Amount Forwarded (Sats) | Fee (Sats) | Funding Mining ||
    // | |                  |                         |                          |            | Fees (Sats)   ||
    // | +------------------+-------------------------+--------------------------+------------+----------------+|
    // | | Taker            | _                      | 500,000                  | _          | 3,000          ||
    // | | Maker16102       | _                      | _                        | _          | _              ||
    // | | Maker6102        | 500,000                | 463,500                  | 33,500     | 3,000          ||
    // | | Maker26102       | 463,500                | 438,642                  | 21,858     | 3,000          ||
    // | +------------------+-------------------------+--------------------------+------------+----------------+|
    // |                                                                                                      |
    // | ### Final Outcomes                                                                                   |
    // |                                                                                                      |
    // | #### Taker (Successful Coinswap):                                                                    |
    // | +-------------+------------------------------------------------------------------------------------+ |
    // | | Participant | Coinswap Outcome (Sats)                                                           | |
    // | +-------------+------------------------------------------------------------------------------------+ |
    // | | Taker       | 438,642 = 500,000 - (Total Fees for Maker16102 + Total Fees for Maker6102)        | |
    // | +-------------+------------------------------------------------------------------------------------+ |
    // |                                                                                                      |
    // | #### Makers:                                                                                        |
    // | +---------------+-----------------------------------------------------------------------------------+|
    // | | Participant    | Coinswap Outcome (Sats)                                                        | |
    // | +---------------+-----------------------------------------------------------------------------------+|
    // | | Maker16102     | 0 (Marked as a bad Maker by Taker)                                              | |
    // | | Maker6102      | 500,000 - 463,500 - 3,000 = +33,500                                             | |
    // | | Maker26102     | 463,500 - 438,642 - 3,000 = +21,858                                             | |
    // | +---------------+-----------------------------------------------------------------------------------+|
    // |                                                                                                      |
    // +------------------------------------------------------------------------------------------------------+

    // Maker might not get banned as Taker may not try 16102 for swap. If it does then check its 16102.
    if !taker.get_bad_makers().is_empty() {
        assert_eq!(
            format!("127.0.0.1:{}", 16102),
            taker.get_bad_makers()[0].address.to_string()
        );
    }

    // After Swap checks:
    verify_swap_results(
        &taker,
        &makers,
        org_taker_spend_balance,
        org_maker_spend_balances,
    );

    info!("Balance check successful.");

    // Check spending from swapcoins.
    info!("Checking Spend from Swapcoin");

    let taker_wallet_mut = taker.get_wallet_mut();

    let swap_coins = taker_wallet_mut
        .list_incoming_swap_coin_utxo_spend_info()
        .unwrap();

    let addr = taker_wallet_mut.get_next_internal_addresses(1).unwrap()[0].to_owned();

    let tx = taker_wallet_mut
        .spend_from_wallet(DEFAULT_TX_FEE_RATE, Destination::Sweep(addr), &swap_coins)
        .unwrap();

    assert_eq!(
        tx.input.len(),
        3,
        "Not all swap coin utxos got included in the spend transaction"
    );

    bitcoind.client.send_raw_transaction(&tx).unwrap();
    generate_blocks(bitcoind, 1);

    taker_wallet_mut.sync().unwrap();

    let balances = taker_wallet_mut.get_balances().unwrap();

    assert_eq!(balances.swap, Amount::ZERO);
    assert_eq!(balances.regular, Amount::from_btc(0.14934642).unwrap());

    info!("All checks successful. Terminating integration test case");

    test_framework.stop();

    block_generation_handle.join().unwrap();
}

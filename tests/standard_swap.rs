#![cfg(feature = "integration-test")]
use bitcoin::Amount;
use coinswap::{
    maker::{start_maker_server, MakerBehavior},
    taker::{SwapParams, TakerBehavior},
    utill::MIN_FEE_RATE,
    wallet::{AddressType, Destination},
};
use std::sync::Arc;

use bitcoind::bitcoincore_rpc::RpcApi;

mod test_framework;
use test_framework::*;

use log::{info, warn};
use std::{sync::atomic::Ordering::Relaxed, thread, time::Duration};

/// This test demonstrates a standard coinswap round between a Taker and 2 Makers. Nothing goes wrong
/// and the coinswap completes successfully.
#[test]
fn test_standard_coinswap() {
    // ---- Setup ----

    // 2 Makers with Normal behavior.
    let makers_config_map = [
        ((6102, Some(19051)), MakerBehavior::Normal),
        ((16102, Some(19052)), MakerBehavior::Normal),
    ];

    let taker_behavior = vec![TakerBehavior::Normal];

    // Initiate test framework, Makers and a Taker with default behavior.
    let (test_framework, mut takers, makers, block_generation_handle) =
        TestFramework::init(makers_config_map.into(), taker_behavior);

    warn!("🧪 Running Test: Standard Coinswap Procedure");
    let bitcoind = &test_framework.bitcoind;

    info!("💰 Funding taker and makers");
    // Fund the Taker  with 3 utxos of 0.05 btc each and do basic checks on the balance
    let taker = &mut takers[0];
    let org_taker_spend_balance =
        fund_and_verify_taker(taker, bitcoind, 3, Amount::from_btc(0.05).unwrap());

    // Fund the Maker with 4 utxos of 0.05 btc each and do basic checks on the balance.
    let makers_ref = makers.iter().map(Arc::as_ref).collect::<Vec<_>>();
    fund_and_verify_maker(makers_ref, bitcoind, 4, Amount::from_btc(0.05).unwrap());

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

            verify_maker_pre_swap_balances(&balances, 14999500);

            balances.spendable
        })
        .collect::<Vec<_>>();

    // Initiate Coinswap
    info!("🔄 Initiating coinswap protocol");

    // Swap params for coinswap.
    let swap_params = SwapParams {
        send_amount: Amount::from_sat(500000),
        maker_count: 2,
        manually_selected_outpoints: None,
    };
    taker.do_coinswap(swap_params).unwrap();

    info!("🎯 All coinswaps processed successfully. Transaction complete.");

    thread::sleep(Duration::from_secs(10));

    //-------- Fee Tracking and Workflow:------------
    //
    // | Participant    | Amount Received (Sats) | Amount Forwarded (Sats) | Fee (Sats) | Total Fees (Sats) |
    // |----------------|------------------------|-------------------------|------------|-------------------|
    // | **Taker**      | _                      | 500,000                 | _          | 1,179(mining fees)|
    // | **Maker16102** | 500,000                | 478,007                 | 21,992     | 21,992            |
    // | **Maker6102**  | 478,007                | 443,633                 | 33,500     | 33,500            |

    //| Component        | Fees (sats) |
    //|------------------|-------------|
    //| **Total**        | 56,671      |

    // ## 3. Final Outcome for Taker (Successful Coinswap):
    //
    // | Participant   | Coinswap Outcome (Sats)                                                                 |
    // |---------------|-----------------------------------------------------------------------------------------|
    // | **Taker**     | 443,633 = 500,000 - (Total Fees for Maker16102 + Total Fees for Maker6102 + mining fees)|
    //
    // ## 4. Final Outcome for Makers:
    //
    // | Participant    | Coinswap Outcome (Sats)                                           |
    // |----------------|-------------------------------------------------------------------|
    // | **Maker16102** | 500,000 − 478,007 = +21,992                                       |
    // | **Maker6102**  | 479,859 − 443,633 = +33,500                                       |

    info!("📊 Verifying swap results");
    // Check Taker balances
    {
        let wallet = taker.get_wallet();
        let balances = wallet.get_balances().unwrap();

        // Debug logging for taker
        log::info!(
            "🔍 DEBUG Taker - Regular: {}, Swap: {}, Spendable: {},Contract: {}",
            balances.regular.to_btc(),
            balances.swap.to_btc(),
            balances.spendable.to_btc(),
            balances.contract.to_btc()
        );
        assert_in_range!(
            balances.regular.to_sat(),
            [
                14499696 // Successful coinswap
            ],
            "Taker seed balance mismatch"
        );

        assert_in_range!(
            balances.swap.to_sat(),
            [
                443633 // Successful coinswap
            ],
            "Taker swapcoin balance mismatch"
        );

        assert_in_range!(balances.contract.to_sat(), [0], "Contract balance mismatch");
        assert_eq!(balances.fidelity, Amount::ZERO);

        // Check balance difference
        let balance_diff = org_taker_spend_balance
            .checked_sub(balances.spendable)
            .unwrap();

        log::info!(
            "🔍 DEBUG Taker balance diff: {} sats",
            balance_diff.to_sat()
        );
        assert_in_range!(
            balance_diff.to_sat(),
            [
                56671  // Fee spent on successful coinswap
            ],
            "Taker spendable balance change mismatch"
        );
    }

    // Check Maker balances
    makers
        .iter()
        .zip(org_maker_spend_balances.iter())
        .enumerate()
        .for_each(|(maker_index, (maker, org_spend_balance))| {
            let mut wallet = maker.get_wallet().write().unwrap();
            wallet.sync_and_save().unwrap();
            let balances = wallet.get_balances().unwrap();

            // Debug logging for makers
            log::info!(
                "🔍 DEBUG Maker {} - Regular: {}, Swap: {}, Contract: {}, Spendable: {}",
                maker_index,
                balances.regular.to_btc(),
                balances.swap.to_btc(),
                balances.contract.to_btc(),
                balances.spendable.to_btc()
            );

            assert_in_range!(
                balances.regular.to_sat(),
                [
                    14555287, // First maker on successful coinswap
                    14533002, // Second maker on successful coinswap
                ],
                "Maker seed balance mismatch"
            );

            assert_in_range!(
                balances.swap.to_sat(),
                [
                    465918, // First maker
                    499724, // Second maker
                ],
                "Maker swapcoin balance mismatch"
            );
            assert_eq!(balances.fidelity, Amount::from_btc(0.05).unwrap());
            // Check spendable balance difference.
            let balance_diff = match org_spend_balance.checked_sub(balances.spendable) {
                None => balances.spendable.checked_sub(*org_spend_balance).unwrap(), // Successful swap as Makers balance increase by Coinswap fee.
                Some(diff) => diff, // No spending or unsuccessful swap , Maker may have lost some funds here, generally due to timelock recovery transaction
            };

            log::info!(
                "🔍 DEBUG Maker {} balance diff: {} sats",
                maker_index,
                balance_diff.to_sat()
            );

            assert_in_range!(
                balance_diff.to_sat(),
                [
                    21705, // First maker fee gained
                    33226, // Second maker fee gained
                ],
                "Maker spendable balance change mismatch"
            );
        });

    log::info!("✅ Swap results verification complete");
    // Check spending from swapcoins.
    info!("💸 Checking spend from swapcoins");

    let taker_wallet_mut = taker.get_wallet_mut();
    let swap_coins = taker_wallet_mut.list_swept_incoming_swap_utxos();

    let addr = taker_wallet_mut
        .get_next_internal_addresses(1, AddressType::P2WPKH)
        .unwrap()[0]
        .to_owned();

    let tx = taker_wallet_mut
        .spend_from_wallet(MIN_FEE_RATE, Destination::Sweep(addr), &swap_coins)
        .unwrap();

    assert_eq!(
        tx.input.len(),
        1,
        "Not all swap coin utxos got included in the spend transaction"
    );

    bitcoind.client.send_raw_transaction(&tx).unwrap();
    generate_blocks(bitcoind, 1);

    taker_wallet_mut.sync_and_save().unwrap();

    let balances = taker_wallet_mut.get_balances().unwrap();

    assert_in_range!(balances.swap.to_sat(), [443415], "Swap Balance Mismatch");
    assert_in_range!(
        balances.regular.to_sat(),
        [14499696],
        "Taker regular balance mismatch"
    );

    info!("🎉 All checks successful. Terminating integration test case");

    // After all balance checks are complete, shut down maker threads
    makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    test_framework.stop();
    block_generation_handle.join().unwrap();
}

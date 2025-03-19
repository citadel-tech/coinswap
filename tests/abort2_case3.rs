#![cfg(feature = "integration-test")]
use bitcoin::Amount;
use coinswap::{
    maker::{start_maker_server, MakerBehavior},
    taker::SwapParams,
    utill::ConnectionType,
};
use std::sync::Arc;
mod test_framework;
use coinswap::taker::TakerBehavior;
use log::{info, warn};
use std::{sync::atomic::Ordering::Relaxed, thread, time::Duration};
use test_framework::*;

/// ABORT 2: Maker Drops Before Setup
/// This test demonstrates the situation where a Maker prematurely drops connections after doing
/// initial protocol handshake. This should not necessarily disrupt the round, the Taker will try to find
/// more makers in his address book and carry on as usual. The Taker will mark this Maker as "bad" and will
/// not swap this maker again.
///
/// CASE 3: Maker Drops After Sending Sender's Signature. Taker and other Maker recovers.
#[test]
fn maker_drops_after_sending_senders_sigs() {
    // ---- Setup ----

    // 6102 is naughty. And theres not enough makers.
    let makers_config_map = [
        ((6102, None), MakerBehavior::CloseAtProofOfFunding),
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

    warn!(
        "Running Test: Maker 6102 Closes after sending sender's signature. This is really bad. Recovery is the only option."
    );

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

            let balances = wallet.get_balances().unwrap();

            assert_eq!(balances.regular, Amount::from_btc(0.14999).unwrap());
            assert_eq!(balances.fidelity, Amount::from_btc(0.05).unwrap());
            assert_eq!(balances.swap, Amount::ZERO);
            assert_eq!(balances.contract, Amount::ZERO);

            balances.spendable
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

    info!("All coinswaps processed successfully. Transaction complete.");

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
    // Case 1: Taker recovers from initiated swap with Maker6102 (CloseAtProofOfFunding)

    //
    // | Participant    | Amount Received (Sats) | Amount Forwarded (Sats) | Fee (Sats) | Funding Mining Fees (Sats) | Total Fees (Sats) |
    // |----------------|------------------------|-------------------------|------------|----------------------------|-------------------|
    // | **Taker**      | _                      | 500,000                 | _          | 3,000                      | 3,000             |

    // - Taker sends [ReqContractSigsForSender] to Maker6102, Maker6102 responds with signatures.
    // - Taker forwards [ProofOfFunding], but Maker6102 doesn't respond, leading to swap recovery.

    //
    // Final Outcome for Taker (Recover from Swap):
    //
    // | Participant    | Mining Fee for Contract txes (Sats) | Timelock Fee (Sats) | Funding Fee (Sats) | Total Recovery Fees (Sats) |
    // |----------------|------------------------------------|---------------------|--------------------|----------------------------|
    // | **Taker**      | 3,000                              | 768                 | 3,000              | 6,768                      |

    // Taker recovers initial funding but incurs 6,768 sats in mining fees.

    //
    // Final Outcome for Makers (Case 1):
    //
    // | Participant    | Coinswap Outcome (Sats)                 |
    // |----------------|------------------------------------------|
    // | **Maker6102**  | 0 (Bad maker marked by Taker)           |
    // | **Maker16102** | 0

    //-------------------------------------------------------------------------------------------------------------------------------------------------------

    // Case 2: Taker -> Maker16102 -> Maker6102 (CloseAtProofOfFunding)

    //
    // | Participant    | Amount Received (Sats) | Amount Forwarded (Sats) | Fee (Sats) | Funding Mining Fees (Sats) | Total Fees (Sats) |
    // |----------------|------------------------|-------------------------|------------|----------------------------|-------------------|
    // | **Taker**      | _                      | 500,000                 | _          | 3,000                      | 3,000             |
    // | **Maker16102** | 500,000                | 463,500                 | 33,500     | 3,000                      | 36,500            |

    // Maker6102 reaches CloseAtProofOfFunding state, Maker16102 and Taker regain funding but incur total loss of 6,768 sats.

    //
    // Final Outcome for Maker6102:
    //
    // | Participant    | Coinswap Outcome (Sats)                 |
    // |----------------|------------------------------------------|
    // | **Maker6102**  | 0 (Bad maker marked by Taker)           |

    // Final Outcome for Maker16102 and Taker:
    //
    // | Participant    | Mining Fee for Contract txes (Sats) | Timelock Fee (Sats) | Funding Fee (Sats) | Total Recovery Fees (Sats) |
    // |----------------|------------------------------------|---------------------|--------------------|----------------------------|
    // | **Taker**      | 3,000                              | 768                 | 3,000              | 6,768                      |
    // | **Maker16102** | 3,000                              | 768                 | 3,000              | 6,768                      |

    // Maker6102 gets banned for being naughty.
    assert_eq!(
        format!("127.0.0.1:{}", 6102),
        taker.get_bad_makers()[0].address.to_string()
    );

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

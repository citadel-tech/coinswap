//! Integration test: Taproot maker reboot recovery preserves funded swapcoins.
//!
//! Route: Taker -> Maker1 (Normal) -> Maker2 (CloseAtPrivateKeyHandover) -> Taker
//!
//! Scenario:
//! 1. Taker initiates a Taproot coinswap with 2 makers.
//! 2. Maker2 broadcasts its funding transaction and persists unfinished swapcoins.
//! 3. Maker2 closes at private key handover.
//! 4. Maker2 is restarted before idle recovery can write a tracker record.
//! 5. Startup recovery must not discard the persisted swapcoins merely because
//!    it cannot find a matching tracker record.

use bitcoin::Amount;
use coinswap::{
    maker::{start_server, MakerBehavior, MakerServer},
    protocol::common_messages::ProtocolVersion,
    taker::{SwapParams, TakerBehavior},
    wallet::AddressType,
};

use super::test_framework::*;

use log::{info, warn};
use std::{
    sync::{atomic::Ordering::Relaxed, Arc},
    thread,
    time::Duration,
};

/// Test: maker reboot recovery should preserve Taproot swapcoins when funding
/// was broadcast but the maker has not yet persisted an idle-recovery tracker
/// record for the original swap id.
///
/// Generic over the backend: this is the only scenario that restarts a maker
/// mid-swap, so it is the only one exercising the watcher's startup re-subscribe
/// of persisted watches. `electrum_tor.rs` reuses it to cover that path over Tor.
pub(crate) fn run_reboot_recovery<B: TestBackend>() {
    warn!("Running Test: Taproot Maker Reboot Recovery Preserves Funded Swapcoins");

    let makers_config_map = vec![(7602, Some(20601)), (17602, Some(20602))];
    let taker_behavior = vec![TakerBehavior::Normal];
    let maker_behaviors = vec![
        MakerBehavior::Normal,
        MakerBehavior::CloseAtPrivateKeyHandover,
    ];

    let (test_framework, mut takers, makers, block_generation_handle) =
        TestFramework::init::<B>(makers_config_map, taker_behavior, maker_behaviors);

    let bitcoind = &test_framework.bitcoind;
    let taker = takers.get_mut(0).unwrap();

    fund_taker(
        taker,
        bitcoind,
        3,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2TR,
    );
    fund_makers(
        &makers,
        bitcoind,
        4,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2TR,
    );

    info!("Starting Maker servers...");
    let maker_threads = makers
        .iter()
        .map(|maker| {
            let maker_clone = maker.clone();
            thread::spawn(move || {
                start_server(maker_clone).unwrap();
            })
        })
        .collect::<Vec<_>>();

    wait_for_makers_setup(&makers, 120);

    for maker in &makers {
        maker.wallet.write().unwrap().sync_and_save().unwrap();
    }

    let swap_params = SwapParams::new(ProtocolVersion::Taproot, Amount::from_sat(500000), 2)
        .with_tx_count(3)
        .with_required_confirms(1);

    generate_blocks(bitcoind, 1);

    let summary = taker
        .prepare_coinswap(swap_params)
        .expect("Prepare should succeed");
    let swap_result = taker.start_coinswap(&summary.swap_id);
    assert!(
        swap_result.is_err(),
        "Swap should fail due to Maker2 closing at private key handover"
    );
    info!("Swap failed as expected: {:?}", swap_result.err().unwrap());

    let victim = makers[1].clone();
    victim.wallet.write().unwrap().sync_and_save().unwrap();
    let before_outgoing = victim.wallet.read().unwrap().get_outgoing_swapcoins_count();
    let before_incoming = victim.wallet.read().unwrap().get_incoming_swapcoins_count();
    assert!(
        before_outgoing > 0,
        "victim maker should have unfinished outgoing swapcoins before reboot"
    );
    assert!(
        before_incoming > 0,
        "victim maker should have unfinished incoming swapcoins before reboot"
    );

    let victim_config = victim.config.clone();
    info!(
        "Restarting Maker2 before idle recovery: incoming={}, outgoing={}",
        before_incoming, before_outgoing
    );

    makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));
    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    drop(victim);
    drop(makers);

    let restarted = Arc::new(MakerServer::init(victim_config).unwrap());
    let restarted_thread = {
        let maker_clone = restarted.clone();
        thread::spawn(move || {
            start_server(maker_clone).unwrap();
        })
    };

    wait_for_makers_setup(std::slice::from_ref(&restarted), 120);
    thread::sleep(Duration::from_secs(5));

    let after_incoming = restarted
        .wallet
        .read()
        .unwrap()
        .get_incoming_swapcoins_count();
    let log_path = format!("{}/taker/debug.log", test_framework.temp_dir.display());
    // Recovery takes longer under parallel load; wait for the markers instead
    // of asserting at a fixed wall-clock point.
    wait_for_log(&log_path, "Incomplete swaps detected on startup", Duration::from_secs(120));
    wait_for_log(&log_path, "recover_from_swap started", Duration::from_secs(120));
    wait_for_log(&log_path, "Removed outgoing swapcoin", Duration::from_secs(120));
    let log_contents = std::fs::read_to_string(&log_path).unwrap();
    assert!(
        !log_contents.contains("Funding was never broadcast for swap"),
        "reboot recovery took the unsafe discard path"
    );
    let recovered_via_hashlock = log_contents.contains("incoming swapcoins via hashlock");

    restarted.shutdown.store(true, Relaxed);
    restarted_thread.join().unwrap();

    test_framework.stop();
    block_generation_handle.join().unwrap();

    assert!(
        after_incoming > 0 || recovered_via_hashlock,
        "maker reboot recovery lost funded incoming swapcoins without hashlock recovery; before={}, after={}",
        before_incoming,
        after_incoming
    );
}

#[test]
fn test_taproot_maker_reboot_recovery_preserves_funded_swapcoins() {
    run_reboot_recovery::<BitcoindBackend>();
}

//! Stress and race-oriented tests for offerbook sync behavior (taker path).

use bitcoin::Amount;
use bitcoind::bitcoincore_rpc::RpcApi;
use coinswap::{
    maker::{start_server, MakerBehavior},
    taker::{MakerProtocol, MakerState, TakerBehavior},
    wallet::AddressType,
    watch_tower::registry_storage::FileRegistry,
};
use log::warn;
use std::{
    sync::{atomic::Ordering::Relaxed, Arc},
    thread,
    time::{Duration, Instant},
};

use super::test_framework::*;

fn spawn_makers(makers: &[Arc<coinswap::maker::MakerServer>]) -> Vec<thread::JoinHandle<()>> {
    let maker_threads = makers
        .iter()
        .map(|maker| {
            let maker_clone = maker.clone();
            thread::spawn(move || {
                start_server(maker_clone).unwrap();
            })
        })
        .collect::<Vec<_>>();

    wait_for_makers_setup(makers, 120);
    maker_threads
}

fn good_maker_count(taker: &coinswap::taker::Taker) -> usize {
    taker
        .fetch_offers()
        .unwrap()
        .all_makers()
        .into_iter()
        .filter(|m| m.state == MakerState::Good)
        .filter(|m| {
            m.protocol
                .as_ref()
                .map(|p| p.supports(&MakerProtocol::Legacy))
                .unwrap_or(false)
        })
        .count()
}

#[test]
fn test_repeated_manual_sync_is_bounded() {
    warn!("Running Test: Staged maker discovery across repeated syncs ");

    let expected_makers = 11;
    let makers_config_map: Vec<(u16, Option<u16>)> =
        (0..expected_makers).map(|i| (8201 + i, None)).collect();
    let taker_behavior = vec![TakerBehavior::Normal];
    let maker_behaviors: Vec<MakerBehavior> = (0..expected_makers as usize)
        .map(|_| MakerBehavior::Normal)
        .collect();

    let (test_framework, mut takers, makers, block_generation_handle) =
        TestFramework::init(makers_config_map, taker_behavior, maker_behaviors);

    let bitcoind = &test_framework.bitcoind;
    let taker = &mut takers[0];

    // Fund all makers
    fund_makers(
        &makers,
        bitcoind,
        3,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2WPKH,
    );

    // Spawn makers in stages: 2, 1, 3, 5
    let stage_plan = [2usize, 1usize, 3usize, 5usize];
    let mut maker_threads = Vec::new();
    let mut spawned = 0usize;
    let syncs_per_stage = 5usize;

    for stage_size in stage_plan {
        let stage_end = spawned + stage_size;
        for maker in &makers[spawned..stage_end] {
            let maker_clone = Arc::clone(maker);
            maker_threads.push(thread::spawn(move || {
                start_server(maker_clone).unwrap();
            }));
        }

        while !makers[..stage_end]
            .iter()
            .all(|m| m.is_setup_complete.load(Relaxed))
        {
            thread::sleep(Duration::from_millis(300));
        }

        for _ in 0..syncs_per_stage {
            taker
                .sync_offerbook_and_wait()
                .expect("manual sync call should complete");
        }

        spawned = stage_end;
    }

    let good = good_maker_count(taker);
    assert_eq!(
        good, expected_makers as usize,
        "expected {expected_makers} good makers after staged syncs, got {good}"
    );

    // Shutdown
    makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));
    maker_threads.into_iter().for_each(|t| t.join().unwrap());
    test_framework.stop();
    block_generation_handle.join().unwrap();
}

#[test]
fn test_nostr_cursor_persisted_for_local_relay() {
    warn!("Running Test: Nostr cursor persistence for local relay ");

    let makers_config_map = vec![(6302, None)];
    let taker_behavior = vec![TakerBehavior::Normal];

    let (test_framework, mut takers, makers, block_generation_handle) =
        TestFramework::init(makers_config_map, taker_behavior, vec![]);

    let relay_url = &test_framework.nostr_relay_url;
    let bitcoind = &test_framework.bitcoind;
    let taker = &mut takers[0];

    fund_makers(
        &makers,
        bitcoind,
        3,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2WPKH,
    );

    let maker_threads = spawn_makers(&makers);

    let _ = taker.sync_offerbook_and_wait();

    let chain = bitcoind
        .client
        .get_blockchain_info()
        .expect("blockchain info")
        .chain
        .to_string();
    let registry_path = test_framework
        .temp_dir
        .join("taker1")
        .join(".taker_watcher")
        .join(&chain);

    let start = Instant::now();
    let timeout = Duration::from_secs(60);
    while start.elapsed() < timeout {
        let reg = FileRegistry::load(registry_path.clone());
        if reg.load_nostr_cursor(relay_url).is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(300));
    }

    let reg = FileRegistry::load(registry_path);
    let cursor = reg
        .load_nostr_cursor(relay_url)
        .expect("cursor for local relay should be present");
    log::info!("Loaded Nostr cursor for {relay_url}: {cursor}");
    assert!(
        cursor > 0,
        "expected positive cursor timestamp, got {}",
        cursor
    );

    // Shutdown
    makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));
    maker_threads.into_iter().for_each(|t| t.join().unwrap());
    test_framework.stop();
    block_generation_handle.join().unwrap();
}

#[test]
fn test_nostr_cursor_is_monotonic_for_local_relay() {
    warn!("Running Test: Nostr cursor monotonic for local relay ");

    let makers_config_map = vec![(6402, None)];
    let taker_behavior = vec![TakerBehavior::Normal];

    let (test_framework, mut takers, makers, block_generation_handle) =
        TestFramework::init(makers_config_map, taker_behavior, vec![]);

    let relay_url = test_framework.nostr_relay_url.clone();
    let bitcoind = &test_framework.bitcoind;
    let taker = &mut takers[0];

    fund_makers(
        &makers,
        bitcoind,
        3,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2WPKH,
    );

    let maker_threads = spawn_makers(&makers);
    let _ = taker.sync_offerbook_and_wait();

    let chain = bitcoind
        .client
        .get_blockchain_info()
        .expect("blockchain info")
        .chain
        .to_string();
    let registry_path = test_framework
        .temp_dir
        .join("taker1")
        .join(".taker_watcher")
        .join(&chain);

    let reg = FileRegistry::load(registry_path);
    let first = reg
        .load_nostr_cursor(&relay_url)
        .expect("cursor for local relay should be present");

    reg.save_nostr_cursor(&relay_url, first.saturating_sub(1));
    let after_lower = reg
        .load_nostr_cursor(&relay_url)
        .expect("cursor should remain present");
    assert_eq!(after_lower, first, "cursor must not move backwards");

    reg.save_nostr_cursor(&relay_url, first.saturating_add(10));
    let after_higher = reg
        .load_nostr_cursor(&relay_url)
        .expect("cursor should remain present");
    assert_eq!(
        after_higher,
        first.saturating_add(10),
        "cursor should move forward"
    );

    // Shutdown
    makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));
    maker_threads.into_iter().for_each(|t| t.join().unwrap());
    test_framework.stop();
    block_generation_handle.join().unwrap();
}

#![cfg(feature = "integration-test")]
//! Stress and race-oriented tests for offerbook sync behavior (legacy taker path).

use bitcoin::Amount;
use bitcoind::bitcoincore_rpc::RpcApi;
use coinswap::{
    maker::{start_maker_server, Maker, MakerBehavior},
    taker::{
        offers::{MakerProtocol, MakerState},
        Taker, TakerBehavior,
    },
    watch_tower::registry_storage::FileRegistry,
};
use log::warn;
use std::{
    sync::{atomic::Ordering::Relaxed, Arc},
    thread,
    time::{Duration, Instant},
};

mod test_framework;
use test_framework::*;

fn spawn_legacy_makers(test_framework: &Arc<TestFramework>, makers: &[Arc<Maker>]) {
    let maker_threads = makers
        .iter()
        .map(|maker| {
            let maker_clone = maker.clone();
            thread::spawn(move || {
                start_maker_server(maker_clone).unwrap();
            })
        })
        .collect::<Vec<_>>();

    test_framework.register_maker_threads(maker_threads);
    while !makers.iter().all(|m| m.is_setup_complete.load(Relaxed)) {
        thread::sleep(Duration::from_millis(300));
    }
}

fn good_maker_count_for_protocol(taker: &mut Taker, protocol: MakerProtocol) -> usize {
    taker
        .fetch_offers()
        .unwrap()
        .all_makers()
        .into_iter()
        .filter(|m| m.state == MakerState::Good)
        .filter(|m| m.protocol.as_ref() == Some(&protocol))
        .count()
}

#[test]
fn test_repeated_manual_sync_is_bounded() {
    warn!("🧪 Running Test: Staged maker discovery across repeated syncs (legacy)");

    let stage_plan = [2usize, 1usize, 3usize, 5usize];
    let expected_makers = stage_plan.iter().sum::<usize>();
    let makers_config_map = (0..expected_makers)
        .map(|i| {
            (
                (8201 + (i as u16), Some(39151 + i as u16)),
                MakerBehavior::Normal,
            )
        })
        .collect::<Vec<_>>();
    let taker_behavior = vec![TakerBehavior::Normal];
    let (test_framework, mut takers, makers) =
        TestFramework::init(makers_config_map, taker_behavior);

    let bitcoind = &test_framework.bitcoind;
    let taker = &mut takers[0];

    let makers_ref = makers.iter().map(Arc::as_ref).collect::<Vec<_>>();
    fund_and_verify_maker(makers_ref, bitcoind, 3, Amount::from_btc(0.05).unwrap());
    thread::sleep(Duration::from_millis(300));

    let mut maker_threads = Vec::new();
    let mut spawned = 0usize;
    let syncs_per_stage = 5usize;

    for stage_size in stage_plan {
        let stage_end = spawned + stage_size;
        for maker in &makers[spawned..stage_end] {
            let maker_clone = Arc::clone(maker);
            maker_threads.push(thread::spawn(move || {
                start_maker_server(maker_clone).unwrap();
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

    test_framework.register_maker_threads(maker_threads);

    taker.fetch_offers().expect("fetch_offers should complete");

    let good = good_maker_count_for_protocol(taker, MakerProtocol::Legacy);
    assert_eq!(
        good, expected_makers,
        "expected {expected_makers} good legacy makers after staged syncs, got {good}"
    );
}

#[test]
fn test_nostr_cursor_persisted_for_local_relay() {
    warn!("🧪 Running Test: Nostr cursor persistence uses local relay port 8000 (legacy)");

    let makers_config_map = vec![((6302, Some(19251)), MakerBehavior::Normal)];
    let taker_behavior = vec![TakerBehavior::Normal];
    let (test_framework, mut takers, makers) =
        TestFramework::init(makers_config_map, taker_behavior);

    let bitcoind = &test_framework.bitcoind;
    let taker = &mut takers[0];

    let makers_ref = makers.iter().map(Arc::as_ref).collect::<Vec<_>>();
    fund_and_verify_maker(makers_ref, bitcoind, 3, Amount::from_btc(0.05).unwrap());

    spawn_legacy_makers(&test_framework, &makers);

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
        if reg.load_nostr_cursor("ws://127.0.0.1:8000").is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(300));
    }

    let reg = FileRegistry::load(registry_path);
    let cursor = reg
        .load_nostr_cursor("ws://127.0.0.1:8000")
        .expect("cursor for local relay should be present");
    log::info!("Loaded Nostr cursor for ws://127.0.0.1:8000: {cursor}");
    assert!(
        cursor > 0,
        "expected positive cursor timestamp, got {}",
        cursor
    );
}

#[test]
fn test_nostr_cursor_is_monotonic_for_local_relay() {
    warn!("🧪 Running Test: Nostr cursor monotonic for local relay port 8000");

    let makers_config_map = vec![((6402, Some(19351)), MakerBehavior::Normal)];
    let taker_behavior = vec![TakerBehavior::Normal];
    let (test_framework, mut takers, makers) =
        TestFramework::init(makers_config_map, taker_behavior);

    let bitcoind = &test_framework.bitcoind;
    let taker = &mut takers[0];

    let makers_ref = makers.iter().map(Arc::as_ref).collect::<Vec<_>>();
    fund_and_verify_maker(makers_ref, bitcoind, 3, Amount::from_btc(0.05).unwrap());

    spawn_legacy_makers(&test_framework, &makers);
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
    let key = "ws://127.0.0.1:8000";
    let first = reg
        .load_nostr_cursor(key)
        .expect("cursor for local relay should be present");

    reg.save_nostr_cursor(key, first.saturating_sub(1));
    let after_lower = reg
        .load_nostr_cursor(key)
        .expect("cursor should remain present");
    assert_eq!(after_lower, first, "cursor must not move backwards");

    reg.save_nostr_cursor(key, first.saturating_add(10));
    let after_higher = reg
        .load_nostr_cursor(key)
        .expect("cursor should remain present");
    assert_eq!(
        after_higher,
        first.saturating_add(10),
        "cursor should move forward"
    );
}

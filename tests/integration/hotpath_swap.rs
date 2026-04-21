#![cfg(feature = "integration-test")]
use crate::test_framework;

use bitcoin::Amount;
use coinswap::{
    maker::{start_server, MakerBehavior},
    protocol::common_messages::ProtocolVersion,
    taker::{SwapParams, TakerBehavior},
    wallet::AddressType,
};
use std::{env, path::PathBuf, sync::atomic::Ordering::Relaxed, thread};

#[test]
fn hotpath_swap_split_report() {
    // We want one full report from a single swap, then split it into per-module
    // reports for CI artifact upload.
    #[cfg(feature = "hotpath")]
    let (hotpath_run, full_report_path, maker_report_path, taker_report_path) = {
        let full_report_path = env::var_os("HOTPATH_OUTPUT_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis();
                std::env::temp_dir().join(format!("coinswap_hotpath_full_{ts}.json"))
            });

        let maker_report_path = env::var_os("COINSWAP_HOTPATH_MAKER_OUTPUT_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| full_report_path.with_file_name("maker.json").to_path_buf());
        let taker_report_path = env::var_os("COINSWAP_HOTPATH_TAKER_OUTPUT_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| full_report_path.with_file_name("taker.json").to_path_buf());

        let hotpath_run = coinswap::hotpath_local::HotpathRun::start(
            "hotpath_swap_split_report",
            full_report_path.clone(),
        )
        .expect("hotpath should start");

        (
            hotpath_run,
            full_report_path,
            maker_report_path,
            taker_report_path,
        )
    };

    let makers_config_map = vec![(7102, Some(19061)), (17102, Some(19062))];
    let taker_behavior = vec![TakerBehavior::Normal];
    let maker_behaviors = vec![MakerBehavior::Normal, MakerBehavior::Normal];

    let (test_framework, mut takers, makers, block_generation_handle) =
        test_framework::TestFramework::init(makers_config_map, taker_behavior, maker_behaviors);

    let bitcoind = &test_framework.bitcoind;
    let taker = takers.get_mut(0).expect("taker must exist");

    let _taker_spendable = test_framework::fund_taker(
        taker,
        bitcoind,
        3,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2TR,
    );

    test_framework::fund_makers(
        &makers,
        bitcoind,
        4,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2TR,
    );

    let maker_threads = makers
        .iter()
        .map(|maker| {
            let maker_clone = maker.clone();
            thread::spawn(move || start_server(maker_clone).expect("maker server should start"))
        })
        .collect::<Vec<_>>();

    test_framework::wait_for_makers_setup(&makers, 120);

    let swap_params = SwapParams::new(ProtocolVersion::Taproot, Amount::from_sat(500_000), 2)
        .with_tx_count(3)
        .with_required_confirms(1);

    test_framework::generate_blocks(bitcoind, 1);

    let summary = taker
        .prepare_coinswap(swap_params)
        .expect("prepare_coinswap must succeed");

    taker
        .start_coinswap(&summary.swap_id)
        .expect("start_coinswap must succeed");

    makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    for t in maker_threads {
        t.join().expect("maker thread must join");
    }

    test_framework.stop();
    block_generation_handle
        .join()
        .expect("block generation thread must join");

    #[cfg(feature = "hotpath")]
    {
        drop(hotpath_run);

        coinswap::hotpath_local::write_filtered_report_by_prefixes(
            &full_report_path,
            &maker_report_path,
            &["coinswap::maker"],
        )
        .expect("maker filtered report should be written");
        coinswap::hotpath_local::write_filtered_report_by_prefixes(
            &full_report_path,
            &taker_report_path,
            &["coinswap::taker"],
        )
        .expect("taker filtered report should be written");

        println!(
            "\n[hotpath] full JSON report: {}",
            full_report_path.display()
        );
        println!(
            "[hotpath] maker JSON report: {}",
            maker_report_path.display()
        );
        coinswap::hotpath_local::print_hotpath_tables_from_path(&maker_report_path);

        println!(
            "\n[hotpath] taker JSON report: {}",
            taker_report_path.display()
        );
        coinswap::hotpath_local::print_hotpath_tables_from_path(&taker_report_path);
    }
}

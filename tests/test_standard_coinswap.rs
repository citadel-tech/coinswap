#![cfg(feature = "integration-test")]
use bitcoin::Amount;
use coinswap::{
    maker::MakerBehavior,
    taker::TakerBehavior,
    test_commons::*,
    wallet::{fidelity::YearAndMonth, RPCConfig, Wallet, WalletMode},
};

use std::{
    fs,
    path::PathBuf,
    sync::{Arc, RwLock},
    thread,
    time::Duration,
};

use std::str::FromStr;

/// This test demonstrates a standard coinswap round between a Taker and 2 Makers. Nothing goes wrong
/// and the coinswap completes successfully.
#[tokio::test]
async fn test_standard_coinswap() {
    coinswap::scripts::setup_logger();

    let test_framework = Arc::new(TestFrameWork::new(None));

    let rpc_config = RPCConfig::from(test_framework.as_ref());

    let mut taker_rpc_config = rpc_config.clone();
    taker_rpc_config.wallet_name = "taker".to_string();

    let mut maker1_rpc_config = rpc_config.clone();
    maker1_rpc_config.wallet_name = "maker_1".to_string();

    let mut maker2_rpc_config = rpc_config.clone();
    maker2_rpc_config.wallet_name = "maker_2".to_string();

    // // unlock all utxos to avoid "insufficient fund" error
    // rpc.call::<Value>("lockunspent", &[Value::Bool(true)])
    //     .unwrap();

    // create temp dir to hold wallet and .dat files if not exists
    if !std::path::Path::new(TEMP_FILES_DIR).exists() {
        fs::create_dir::<PathBuf>(TEMP_FILES_DIR.into()).unwrap();
    }

    // create taker wallet
    let mut taker_wallet = create_wallet_and_import(TAKER.into(), &taker_rpc_config);

    // create maker1 wallet
    let mut maker1_wallet = create_wallet_and_import(MAKER1.into(), &maker1_rpc_config);

    // create maker2 wallet
    let mut maker2_wallet = create_wallet_and_import(MAKER2.into(), &maker2_rpc_config);

    // Check files are created
    assert!(std::path::Path::new(TAKER).exists());
    assert!(std::path::Path::new(MAKER1).exists());
    assert!(std::path::Path::new(MAKER2).exists());

    // Create 3 taker and maker address and send 0.05 btc to each
    for _ in 0..3 {
        let taker_address = taker_wallet.get_next_external_address().unwrap();
        let maker1_address = maker1_wallet.get_next_external_address().unwrap();
        let maker2_address = maker2_wallet.get_next_external_address().unwrap();

        test_framework.send_to_address(&taker_address, Amount::from_btc(0.05).unwrap());
        test_framework.send_to_address(&maker1_address, Amount::from_btc(0.05).unwrap());
        test_framework.send_to_address(&maker2_address, Amount::from_btc(0.05).unwrap());
    }

    // Create a fidelity bond for each maker
    let maker1_fbond_address = maker1_wallet
        .get_timelocked_address(&YearAndMonth::new(2030, 1))
        .0;
    let maker2_fbond_address = maker2_wallet
        .get_timelocked_address(&YearAndMonth::new(2030, 1))
        .0;

    test_framework.send_to_address(&maker1_fbond_address, Amount::from_btc(0.05).unwrap());
    test_framework.send_to_address(&maker2_fbond_address, Amount::from_btc(0.05).unwrap());

    test_framework.generate_1_block();

    // Check inital wallet assertions
    assert_eq!(*taker_wallet.get_external_index(), 3);
    assert_eq!(*maker1_wallet.get_external_index(), 3);
    assert_eq!(*maker2_wallet.get_external_index(), 3);

    assert_eq!(
        taker_wallet
            .list_unspent_from_wallet(false, true)
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        maker1_wallet
            .list_unspent_from_wallet(false, true)
            .unwrap()
            .len(),
        4
    );
    assert_eq!(
        maker2_wallet
            .list_unspent_from_wallet(false, true)
            .unwrap()
            .len(),
        4
    );

    assert_eq!(taker_wallet.lock_all_nonwallet_unspents().unwrap(), ());
    assert_eq!(maker1_wallet.lock_all_nonwallet_unspents().unwrap(), ());
    assert_eq!(maker2_wallet.lock_all_nonwallet_unspents().unwrap(), ());

    let kill_flag = Arc::new(RwLock::new(false));

    let maker1_config_clone = maker1_rpc_config.clone();
    let kill_flag_maker_1 = kill_flag.clone();
    let maker1_thread = thread::spawn(move || {
        coinswap::scripts::maker::run_maker(
            &PathBuf::from_str(MAKER1).unwrap(),
            &maker1_config_clone,
            6102,
            Some(WalletMode::Testing),
            MakerBehavior::Normal,
            kill_flag_maker_1,
        )
        .unwrap();
    });

    let maker2_config_clone = maker2_rpc_config.clone();
    let kill_flag_maker_2 = kill_flag.clone();
    let maker2_thread = thread::spawn(move || {
        coinswap::scripts::maker::run_maker(
            &PathBuf::from_str(MAKER2).unwrap(),
            &maker2_config_clone,
            16102,
            Some(WalletMode::Testing),
            MakerBehavior::Normal,
            kill_flag_maker_2,
        )
        .unwrap();
    });

    let taker_config_clone = taker_rpc_config.clone();
    let taker_thread = thread::spawn(|| {
        // Wait and then start the taker
        thread::sleep(Duration::from_secs(20));
        coinswap::scripts::taker::run_taker(
            &PathBuf::from_str(TAKER).unwrap(),
            Some(WalletMode::Testing),
            Some(taker_config_clone), /* Default RPC */
            1000,
            500000,
            2,
            3,
            TakerBehavior::Normal,
        );
    });

    let test_frameowrk_ptr = test_framework.clone();
    let kill_block_creation_clone = kill_flag.clone();
    let block_creation_thread = thread::spawn(move || {
        while !*kill_block_creation_clone.read().unwrap() {
            thread::sleep(Duration::from_secs(5));
            test_frameowrk_ptr.generate_1_block();
            log::info!("created block");
        }
        log::info!("ending block creation thread");
    });

    taker_thread.join().unwrap();
    *kill_flag.write().unwrap() = true;
    maker1_thread.join().unwrap();
    maker2_thread.join().unwrap();
    block_creation_thread.join().unwrap();

    // Recreate the wallet
    let taker_wallet =
        Wallet::load(&taker_rpc_config, &TAKER.into(), Some(WalletMode::Testing)).unwrap();
    let maker1_wallet = Wallet::load(
        &maker1_rpc_config,
        &MAKER1.into(),
        Some(WalletMode::Testing),
    )
    .unwrap();
    let maker2_wallet = Wallet::load(
        &maker2_rpc_config,
        &MAKER2.into(),
        Some(WalletMode::Testing),
    )
    .unwrap();

    // Check assertions
    assert_eq!(taker_wallet.get_swapcoins_count(), 6);
    assert_eq!(maker1_wallet.get_swapcoins_count(), 6);
    assert_eq!(maker2_wallet.get_swapcoins_count(), 6);

    let utxos = taker_wallet.list_unspent_from_wallet(false, false).unwrap();
    let balance: Amount = utxos
        .iter()
        .fold(Amount::ZERO, |acc, (u, _)| acc + u.amount);
    assert_eq!(utxos.len(), 6);
    assert!(balance < Amount::from_btc(0.15).unwrap());

    let utxos = maker1_wallet
        .list_unspent_from_wallet(false, false)
        .unwrap();
    let balance: Amount = utxos
        .iter()
        .fold(Amount::ZERO, |acc, (u, _)| acc + u.amount);
    assert_eq!(utxos.len(), 6);
    assert!(balance > Amount::from_btc(0.15).unwrap());

    let utxos = maker2_wallet
        .list_unspent_from_wallet(false, false)
        .unwrap();
    let balance: Amount = utxos
        .iter()
        .fold(Amount::ZERO, |acc, (u, _)| acc + u.amount);
    assert_eq!(utxos.len(), 6);
    assert!(balance > Amount::from_btc(0.15).unwrap());

    // Remove test temp files and dir
    fs::remove_dir_all::<PathBuf>(TEMP_FILES_DIR.into()).unwrap();

    test_framework.stop();
}

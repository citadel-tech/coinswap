#![cfg(feature = "integration-test")]
use bitcoin::Amount;
use coinswap::{
    maker::MakerBehavior,
    taker::{SwapParams, Taker, TakerBehavior},
    test_commons::*,
    wallet::{RPCConfig, WalletMode},
};
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, RwLock},
    thread,
    time::Duration,
};

/// ABORT 2: Maker Drops Before Setup
/// This test demonstrates the situation where a Maker prematurely drops connections after doing
/// initial protocol handshake. This should not necessarily disrupt the round, the Taker will try to find
/// more makers in his address book and carry on as usual. The Taker will mark this Maker as "bad" and will
/// not swap this maker again.
///
/// CASE 2: Maker Drops Before Sending Sender's Signature, and Taker cannot find a new Maker, recovers from Swap.
#[tokio::test]
async fn test_abort_case_2_recover_if_no_makers_found() {
    coinswap::scripts::setup_logger();

    let test_framework = Arc::new(TestFrameWork::new(None));

    let rpc_config = RPCConfig::from(test_framework.as_ref());

    let mut taker_rpc_config = rpc_config.clone();
    taker_rpc_config.wallet_name = "taker".to_string();

    let mut maker1_rpc_config = rpc_config.clone();
    maker1_rpc_config.wallet_name = "maker_1".to_string();

    let mut maker2_rpc_config = rpc_config.clone();
    maker2_rpc_config.wallet_name = "maker_2".to_string();

    let temp_path = get_random_tmp_dir();

    let taker_path = temp_path.join("taker");
    let maker6102_path = temp_path.join("maker6102");
    let maker16102_path = temp_path.join("maker16102");

    println!("{:?}", taker_path);
    println!("{:?}", maker16102_path);
    println!("{:?}", maker6102_path);

    // Start from fresh temp dir
    if temp_path.exists() {
        fs::remove_dir_all::<PathBuf>(temp_path.clone()).unwrap();
    }

    // create maker1 wallet
    let mut maker1_wallet = create_wallet_and_import(&maker6102_path, &maker1_rpc_config);

    // create maker2 wallet
    let mut maker2_wallet = create_wallet_and_import(&maker16102_path, &maker2_rpc_config);

    let mut taker = Taker::init(
        &taker_path,
        Some(taker_rpc_config),
        Some(WalletMode::Testing),
        TakerBehavior::Normal,
    )
    .await
    .unwrap();

    // Create 3 taker and maker address and send 0.05 btc to each
    for _ in 0..3 {
        let taker_address = taker.get_wallet_mut().get_next_external_address().unwrap();
        let maker1_address = maker1_wallet.get_next_external_address().unwrap();
        let maker2_address = maker2_wallet.get_next_external_address().unwrap();

        test_framework.send_to_address(&taker_address, Amount::from_btc(0.05).unwrap());
        test_framework.send_to_address(&maker1_address, Amount::from_btc(0.05).unwrap());
        test_framework.send_to_address(&maker2_address, Amount::from_btc(0.05).unwrap());
    }

    test_framework.generate_1_block();

    test_framework.generate_1_block();

    // Check initial wallet assertions
    assert_eq!(*taker.get_wallet().get_external_index(), 3);
    assert_eq!(*maker1_wallet.get_external_index(), 3);
    assert_eq!(*maker2_wallet.get_external_index(), 3);

    assert_eq!(
        taker
            .get_wallet()
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
        3
    );
    assert_eq!(
        maker2_wallet
            .list_unspent_from_wallet(false, true)
            .unwrap()
            .len(),
        3
    );

    assert_eq!(
        taker.get_wallet().lock_all_nonwallet_unspents().unwrap(),
        ()
    );
    assert_eq!(maker1_wallet.lock_all_nonwallet_unspents().unwrap(), ());
    assert_eq!(maker2_wallet.lock_all_nonwallet_unspents().unwrap(), ());

    let kill_flag = Arc::new(RwLock::new(false));

    let maker1_config_clone = maker1_rpc_config.clone();
    let kill_flag_maker_1 = kill_flag.clone();
    let maker6102_path_clone = maker6102_path.clone();
    let maker1_thread = thread::spawn(move || {
        coinswap::scripts::maker::run_maker(
            &maker6102_path_clone,
            &maker1_config_clone,
            6102,
            Some(WalletMode::Testing),
            MakerBehavior::CloseBeforeSendingSendersSigs,
            kill_flag_maker_1,
        )
        .unwrap();
    });

    let maker2_config_clone = maker2_rpc_config.clone();
    let kill_flag_maker_2 = kill_flag.clone();
    let maker16102_path_clone = maker16102_path.clone();
    let maker2_thread = thread::spawn(move || {
        coinswap::scripts::maker::run_maker(
            &maker16102_path_clone,
            &maker2_config_clone,
            16102,
            Some(WalletMode::Testing),
            MakerBehavior::Normal,
            kill_flag_maker_2,
        )
        .unwrap();
    });

    let org_maker_2_balance = maker1_wallet.get_wallet_balance().unwrap();
    let org_taker_balance = taker.get_wallet().get_wallet_balance().unwrap();

    let test_frameowrk_ptr = test_framework.clone();
    let kill_block_creation_clone = kill_flag.clone();
    let block_creation_thread = thread::spawn(move || {
        while !*kill_block_creation_clone.read().unwrap() {
            thread::sleep(Duration::from_secs(1));
            test_frameowrk_ptr.generate_1_block();
            log::info!("created block");
        }
        log::info!("ending block creation thread");
    });

    let swap_params = SwapParams {
        send_amount: 500000,
        maker_count: 2,
        tx_count: 3,
        required_confirms: 1,
        fee_rate: 1000,
    };

    thread::sleep(Duration::from_secs(20));
    taker.send_coinswap(swap_params).await.unwrap();
    *kill_flag.write().unwrap() = true;
    maker1_thread.join().unwrap();
    maker2_thread.join().unwrap();

    block_creation_thread.join().unwrap();

    let maker_2_balance = maker2_wallet.get_wallet_balance().unwrap();
    let taker_balance = taker.get_wallet().get_wallet_balance().unwrap();

    // Assert that Taker burned the mining fees,
    // Maker is fine.
    assert_eq!(org_maker_2_balance - maker_2_balance, Amount::from_sat(0));
    assert_eq!(org_taker_balance - taker_balance, Amount::from_sat(4227));

    fs::remove_dir_all::<PathBuf>(TEMP_FILES_DIR.into()).unwrap();
    test_framework.stop();
}

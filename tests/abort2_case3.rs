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
    str::FromStr,
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
/// CASE 3: Maker Drops After Sending Sender's Signature. Taker and other Maker recovers.
#[tokio::test]
async fn maker_drops_after_sending_senders_sigs() {
    coinswap::scripts::setup_logger();

    let test_framework = Arc::new(TestFrameWork::new(None));

    let rpc_config = RPCConfig::from(test_framework.as_ref());

    let mut taker_rpc_config = rpc_config.clone();
    taker_rpc_config.wallet_name = "taker".to_string();

    let mut maker1_rpc_config = rpc_config.clone();
    maker1_rpc_config.wallet_name = "maker_1".to_string();

    let mut maker2_rpc_config = rpc_config.clone();
    maker2_rpc_config.wallet_name = "maker_2".to_string();

    // Start from fresh temp dir
    if PathBuf::from_str(TEMP_FILES_DIR).unwrap().exists() {
        fs::remove_dir_all::<PathBuf>(TEMP_FILES_DIR.into()).unwrap();
    }

    let mut taker = Taker::init(
        &PathBuf::from_str(TAKER).unwrap(),
        Some(taker_rpc_config),
        Some(WalletMode::Testing),
        TakerBehavior::Normal,
    )
    .await
    .unwrap();

    // create maker1 wallet
    let mut maker1_wallet = create_wallet_and_import(&MAKER1.into(), &maker1_rpc_config);

    // create maker2 wallet
    let mut maker2_wallet = create_wallet_and_import(&MAKER2.into(), &maker2_rpc_config);

    // Check files are created
    assert!(std::path::Path::new(TAKER).exists());
    assert!(std::path::Path::new(MAKER1).exists());
    assert!(std::path::Path::new(MAKER2).exists());

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

    // Check inital wallet assertions
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
    let maker1_thread = thread::spawn(move || {
        coinswap::scripts::maker::run_maker(
            &PathBuf::from_str(MAKER1).unwrap(),
            &maker1_config_clone,
            6102,
            Some(WalletMode::Testing),
            MakerBehavior::CloseAfterSendingSendersSigs,
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

    // Maker gets banned for being naughty.
    assert_eq!(
        "localhost:6102",
        taker.get_bad_makers()[0].address.to_string()
    );

    fs::remove_dir_all::<PathBuf>(TEMP_FILES_DIR.into()).unwrap();
    test_framework.stop();
}

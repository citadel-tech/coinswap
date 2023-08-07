use bitcoin::util::amount::Amount;
use bitcoincore_rpc::{Client, RpcApi};

use bip39::Mnemonic;

use teleport::{
    maker::server::MakerBehavior,
    wallet::{fidelity::YearAndMonth, RPCConfig, Wallet, WalletMode},
};

use serde_json::Value;

use std::{
    convert::TryFrom,
    path::PathBuf,
    sync::{Arc, RwLock},
    thread, time,
};

use std::str::FromStr;

static WATCHTOWER_DATA: &str = "tests/watchtower.dat";
static TAKER: &str = "tests/taker-wallet";
static MAKER1: &str = "tests/maker-wallet-1";
static MAKER2: &str = "tests/maker-wallet-2";

// Helper function to create new wallet
fn create_wallet_and_import(filename: PathBuf) -> Wallet {
    let mnemonic = Mnemonic::generate(12).unwrap();
    let seedphrase = mnemonic.to_string();

    let mut wallet = Wallet::init(
        &filename,
        &RPCConfig::default(),
        seedphrase,
        "".to_string(),
        Some(WalletMode::Testing),
    )
    .unwrap();

    wallet.sync().unwrap();

    wallet
}

pub fn generate_1_block(rpc: &Client) {
    rpc.generate_to_address(1, &rpc.get_new_address(None, None).unwrap())
        .unwrap();
}

// This test requires a bitcoin regtest node running in local machine with a
// wallet name `teleport` loaded and have enough balance to execute transactions.
// TODO: Used `bitcoind` crate to automate spawning regtest nodes.
#[tokio::test]
async fn test_standard_coinswap() {
    teleport::scripts::setup_logger();

    let rpc = Client::try_from(&RPCConfig::default()).unwrap();

    // unlock all utxos to avoid "insufficient fund" error
    rpc.call::<Value>("lockunspent", &[Value::Bool(true)])
        .unwrap();

    // create taker wallet
    let mut taker_wallet = create_wallet_and_import(TAKER.into());

    // create maker1 wallet
    let mut maker1_wallet = create_wallet_and_import(MAKER1.into());

    // create maker2 wallet
    let mut maker2_wallet = create_wallet_and_import(MAKER2.into());

    // Check files are created
    assert!(std::path::Path::new(TAKER).exists());
    assert!(std::path::Path::new(MAKER1).exists());
    assert!(std::path::Path::new(MAKER2).exists());

    // Create 3 taker and maker address and send 0.05 btc to each
    for _ in 0..3 {
        let taker_address = taker_wallet.get_next_external_address().unwrap();
        let maker1_address = maker1_wallet.get_next_external_address().unwrap();
        let maker2_address = maker2_wallet.get_next_external_address().unwrap();

        rpc.send_to_address(
            &taker_address,
            Amount::from_btc(0.05).unwrap(),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        rpc.send_to_address(
            &maker1_address,
            Amount::from_btc(0.05).unwrap(),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        rpc.send_to_address(
            &maker2_address,
            Amount::from_btc(0.05).unwrap(),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    }

    // Create a fidelity bond for each maker
    let maker1_fbond_address = maker1_wallet
        .get_timelocked_address(&YearAndMonth::new(2030, 1))
        .0;
    let maker2_fbond_address = maker2_wallet
        .get_timelocked_address(&YearAndMonth::new(2030, 1))
        .0;
    rpc.send_to_address(
        &maker1_fbond_address,
        Amount::from_btc(0.05).unwrap(),
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();
    rpc.send_to_address(
        &maker2_fbond_address,
        Amount::from_btc(0.05).unwrap(),
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();

    generate_1_block(&rpc);

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

    // Start watchtower, makers and taker to execute a coinswap
    let kill_flag_watchtower = kill_flag.clone();
    let watchtower_thread = thread::spawn(|| {
        teleport::scripts::watchtower::run_watchtower(
            &PathBuf::from_str(WATCHTOWER_DATA).unwrap(),
            Some(kill_flag_watchtower),
        )
        .unwrap();
    });

    let kill_flag_maker1 = kill_flag.clone();
    let maker1_thread = thread::spawn(|| {
        teleport::scripts::maker::run_maker(
            &PathBuf::from_str(MAKER1).unwrap(),
            6102,
            Some(WalletMode::Testing),
            MakerBehavior::Normal,
            Some(kill_flag_maker1),
        )
        .unwrap();
    });

    let kill_flag_maker2 = kill_flag.clone();
    let maker2_thread = thread::spawn(|| {
        teleport::scripts::maker::run_maker(
            &PathBuf::from_str(MAKER2).unwrap(),
            16102,
            Some(WalletMode::Testing),
            MakerBehavior::Normal,
            Some(kill_flag_maker2),
        )
        .unwrap();
    });

    let taker_thread = thread::spawn(|| {
        // Wait and then start the taker
        thread::sleep(time::Duration::from_secs(20));
        teleport::scripts::taker::run_taker(
            &PathBuf::from_str(TAKER).unwrap(),
            Some(WalletMode::Testing),
            None, /* Default RPC */
            1000,
            500000,
            2,
            3,
        );
    });

    let kill_flag_block_creation_thread = kill_flag.clone();
    let rpc_ptr = Arc::new(rpc);
    let block_creation_thread = thread::spawn(move || {
        while !*kill_flag_block_creation_thread.read().unwrap() {
            thread::sleep(time::Duration::from_secs(5));
            generate_1_block(&rpc_ptr);
            println!("created block");
        }
        println!("ending block creation thread");
    });

    taker_thread.join().unwrap();
    *kill_flag.write().unwrap() = true;
    maker1_thread.join().unwrap();
    maker2_thread.join().unwrap();
    watchtower_thread.join().unwrap();
    block_creation_thread.join().unwrap();

    // Recreate the wallet
    let taker_wallet = Wallet::load(
        &RPCConfig::default(),
        &TAKER.into(),
        Some(WalletMode::Testing),
    )
    .unwrap();
    let maker1_wallet = Wallet::load(
        &RPCConfig::default(),
        &MAKER1.into(),
        Some(WalletMode::Testing),
    )
    .unwrap();
    let maker2_wallet = Wallet::load(
        &RPCConfig::default(),
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
}

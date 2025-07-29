mod test_framework;

use std::{fs, path::PathBuf};

use bitcoin::{Address, Amount};
use bitcoind::{
    bitcoincore_rpc::{self, Auth},
    BitcoinD,
};
use log::info;

use coinswap::wallet::{
    load_sensitive_struct_interactive, KeyMaterial, RPCConfig, SerdeJson, WalletBackup, WalletError,
};

use test_framework::init_bitcoind;

use crate::test_framework::{generate_blocks, send_to_address};

fn setup(test_name: String) -> (PathBuf, RPCConfig, PathBuf, BitcoinD, PathBuf) {
    let temp_dir = std::env::temp_dir()
        .join("coinswap")
        .join("wallet-tests")
        .join(test_name);
    let wallets_dir = temp_dir.join("");

    let original_wallet_name = "original-wallet".to_string();
    let original_wallet = wallets_dir.join(&original_wallet_name);
    let wallet_backup_file = wallets_dir.join("wallet-backup.json");
    let restored_wallet_name = "restored-wallet".to_string();
    let restored_wallet_file = wallets_dir.join(&restored_wallet_name);
    if temp_dir.exists() {
        fs::remove_dir_all(&temp_dir).unwrap();
    }
    //println!("temporary directory : {}", temp_dir.display());

    let bitcoind = init_bitcoind(&temp_dir);

    let url = bitcoind.rpc_url().split_at(7).1.to_string();
    let auth = Auth::CookieFile(bitcoind.params.cookie_file.clone());

    let rpc_config = RPCConfig {
        url,
        auth,
        wallet_name: original_wallet_name.clone(),
    };
    (
        original_wallet,
        rpc_config,
        wallet_backup_file,
        bitcoind,
        restored_wallet_file,
    )
}

fn cleanup(bitcoind: &mut BitcoinD) {
    bitcoind.stop().unwrap();
}

fn send_and_mine(
    bitcoind: &mut BitcoinD,
    address: &Address,
    btc_amount: f64,
    blocks_to_generate: u64,
) -> Result<(), bitcoincore_rpc::Error> {
    send_to_address(bitcoind, address, Amount::from_btc(btc_amount)?);
    generate_blocks(bitcoind, blocks_to_generate);
    Ok(())
}

#[test]
fn plainwallet_plainbackup_plainrestore() {
    info!("Running Test: Creating Wallet file, backing it up, then recieve a payment, and restore backup");

    let (original_wallet, rpc_config, wallet_backup_file, mut bitcoind, restored_wallet_file) =
        setup("plain_wallet_plainbackup_plain_restore".to_string());

    let mut wallet = coinswap::wallet::Wallet::init(&original_wallet, &rpc_config, None).unwrap();

    let addr = wallet.get_next_external_address().unwrap();
    send_and_mine(&mut bitcoind, &addr, 0.05, 1).unwrap();

    wallet.backup(&wallet_backup_file, None);

    let addr = wallet.get_next_external_address().unwrap();
    send_and_mine(&mut bitcoind, &addr, 0.05, 1).unwrap();

    wallet.sync().unwrap();

    let (backup, _) = load_sensitive_struct_interactive::<WalletBackup, WalletError, SerdeJson>(
        &wallet_backup_file,
    )
    .unwrap();

    let restored_wallet = WalletBackup::restore(&backup, &restored_wallet_file, &rpc_config, None);

    assert_eq!(wallet, restored_wallet); // only compares .store!

    //cleanup(&mut bitcoind);
    cleanup(&mut bitcoind);

    info!("🎉 Wallet Backup and Restore after tx test ran succefully!");
}

#[test]
fn encwallet_encbackup_encrestore() {
    let (original_wallet, rpc_config, wallet_backup_file, mut bitcoind, restored_wallet_file) =
        setup("encwallet_encbackup_encrestore".to_string());

    let km = KeyMaterial::new_interactive(None);

    let mut wallet =
        coinswap::wallet::Wallet::init(&original_wallet, &rpc_config, km.clone()).unwrap();

    let addr = wallet.get_next_external_address().unwrap();
    send_and_mine(&mut bitcoind, &addr, 0.05, 1).unwrap();

    wallet.backup(&wallet_backup_file, km.clone());

    let addr = wallet.get_next_external_address().unwrap();
    send_and_mine(&mut bitcoind, &addr, 0.05, 1).unwrap();

    wallet.sync().unwrap();

    let (backup, _) = load_sensitive_struct_interactive::<WalletBackup, WalletError, SerdeJson>(
        &wallet_backup_file,
    )
    .unwrap();

    let restored_wallet =
        WalletBackup::restore(&backup, &restored_wallet_file, &rpc_config, km.clone());

    assert_eq!(wallet, restored_wallet); // only compares .store!

    cleanup(&mut bitcoind);
}

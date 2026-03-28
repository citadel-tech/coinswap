use std::{
    fs,
    path::{Path, PathBuf},
};

use bip39::rand;
use bitcoin::{Address, Amount};
use bitcoind::{
    bitcoincore_rpc::{self, Auth},
    BitcoinD,
};
use log::info;

use coinswap::wallet::{AddressType, RPCConfig, Wallet, WalletBackup};

use coinswap::security::{load_sensitive_struct, KeyMaterial, SerdeJson};

use super::test_framework::init_bitcoind;

use super::test_framework::{generate_blocks, send_to_address};

fn setup(test_name: String) -> (PathBuf, RPCConfig, PathBuf, BitcoinD, PathBuf, PathBuf) {
    let root_dir = std::env::temp_dir().join(format!("coinswap-{}", rand::random::<u64>()));
    let temp_dir = root_dir.join("wallet-tests").join(test_name);
    let wallets_dir = temp_dir.join("");

    let original_wallet_name = "original-wallet".to_string();
    let original_wallet = wallets_dir.join(&original_wallet_name);
    let wallet_backup_file = wallets_dir.join("wallet-backup.json");
    let restored_wallet_name = "restored-wallet".to_string();
    let restored_wallet_file = wallets_dir.join(&restored_wallet_name);
    if temp_dir.exists() {
        fs::remove_dir_all(&temp_dir).unwrap();
    }

    let port_zmq = 28332 + rand::random::<u16>() % 1000;

    let zmq_addr = format!("tcp://127.0.0.1:{port_zmq}");

    let bitcoind = init_bitcoind(&temp_dir, zmq_addr);

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
        root_dir,
    )
}

fn cleanup(bitcoind: &mut BitcoinD, root_dir: &Path) {
    bitcoind.stop().unwrap();
    std::thread::sleep(std::time::Duration::from_secs(2));
    if root_dir.exists() {
        let _ = fs::remove_dir_all(root_dir);
    }
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
    info!("Running Test: Creating Wallet file, backing it up, then receive a payment, and restore backup");

    let (
        original_wallet,
        rpc_config,
        wallet_backup_file,
        mut bitcoind,
        restored_wallet_file,
        root_dir,
    ) = setup("plain_wallet_plainbackup_plain_restore".to_string());

    let mut wallet = coinswap::wallet::Wallet::init(&original_wallet, &rpc_config, None).unwrap();

    let addr = wallet.get_next_external_address(AddressType::P2TR).unwrap();
    send_and_mine(&mut bitcoind, &addr, 0.05, 1).unwrap();

    let _ = wallet.backup(&wallet_backup_file, None);

    let addr = wallet.get_next_external_address(AddressType::P2TR).unwrap();
    send_and_mine(&mut bitcoind, &addr, 0.05, 1).unwrap();

    wallet.sync_and_save().unwrap();

    let (backup, _) = load_sensitive_struct::<WalletBackup, SerdeJson>(&wallet_backup_file, None);

    let restored_wallet =
        Wallet::restore(&backup, &restored_wallet_file, &rpc_config, None).unwrap();

    assert_eq!(wallet, restored_wallet); // only compares .store!

    cleanup(&mut bitcoind, &root_dir);

    info!("🎉 Wallet Backup and Restore after tx test ran successfully!");
}

#[test]
fn encwallet_encbackup_encrestore() {
    let (
        original_wallet,
        rpc_config,
        wallet_backup_file,
        mut bitcoind,
        restored_wallet_file,
        root_dir,
    ) = setup("encwallet_encbackup_encrestore".to_string());

    let km = KeyMaterial::new_interactive(None);

    let mut wallet =
        coinswap::wallet::Wallet::init(&original_wallet, &rpc_config, km.clone()).unwrap();

    let addr = wallet.get_next_external_address(AddressType::P2TR).unwrap();
    send_and_mine(&mut bitcoind, &addr, 0.05, 1).unwrap();

    let _ = wallet.backup(&wallet_backup_file, km.clone());

    let addr = wallet.get_next_external_address(AddressType::P2TR).unwrap();
    send_and_mine(&mut bitcoind, &addr, 0.05, 1).unwrap();

    wallet.sync_and_save().unwrap();

    let (backup, _) = load_sensitive_struct::<WalletBackup, SerdeJson>(&wallet_backup_file, None);

    let restored_wallet =
        Wallet::restore(&backup, &restored_wallet_file, &rpc_config, km.clone()).unwrap();

    assert_eq!(wallet, restored_wallet); // only compares .store!

    cleanup(&mut bitcoind, &root_dir);
}

use std::{
    fs,
    path::{Path, PathBuf},
};

use bip39::rand;
use bitcoin::{Address, Amount};
use bitcoind::{
    bitcoincore_rpc::{self, Auth, RpcApi},
    BitcoinD,
};
use electrsd::ElectrsD;
use log::info;

use coinswap::wallet::{
    AddressType, AnyBlockchain, BackendConfig, Blockchain, CoreRPC, CoreRpcConfig, Electrum,
    ElectrumConfig, Wallet, WalletBackup,
};

use coinswap::security::{load_sensitive_struct, KeyMaterial, SerdeJson};

use super::test_framework::{generate_blocks, init_bitcoind, init_electrsd, send_to_address};

fn setup(test_name: String) -> (PathBuf, CoreRpcConfig, PathBuf, BitcoinD, PathBuf, PathBuf) {
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

    let rpc_config = CoreRpcConfig {
        url,
        auth,
        wallet_name: original_wallet_name.clone(),
        ..CoreRpcConfig::default()
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

    let mut wallet = Wallet::init(
        &original_wallet,
        AnyBlockchain::CoreRPC(CoreRPC::new(&rpc_config).unwrap()),
        None,
    )
    .unwrap();

    let addr = wallet.get_next_external_address(AddressType::P2TR).unwrap();
    send_and_mine(&mut bitcoind, &addr, 0.05, 1).unwrap();

    let _ = wallet.backup(&wallet_backup_file, None);

    let addr = wallet.get_next_external_address(AddressType::P2TR).unwrap();
    send_and_mine(&mut bitcoind, &addr, 0.05, 1).unwrap();

    wallet.sync_and_save().unwrap();

    let (backup, _) = load_sensitive_struct::<WalletBackup, SerdeJson>(&wallet_backup_file, None);

    let restored_wallet = Wallet::restore(
        &backup,
        &restored_wallet_file,
        &BackendConfig::CoreRpc(rpc_config.clone()),
        None,
    )
    .unwrap();

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

    let mut wallet = Wallet::init(
        &original_wallet,
        AnyBlockchain::CoreRPC(CoreRPC::new(&rpc_config).unwrap()),
        km.clone(),
    )
    .unwrap();

    let addr = wallet.get_next_external_address(AddressType::P2TR).unwrap();
    send_and_mine(&mut bitcoind, &addr, 0.05, 1).unwrap();

    let _ = wallet.backup(&wallet_backup_file, km.clone());

    let addr = wallet.get_next_external_address(AddressType::P2TR).unwrap();
    send_and_mine(&mut bitcoind, &addr, 0.05, 1).unwrap();

    wallet.sync_and_save().unwrap();

    let (backup, _) = load_sensitive_struct::<WalletBackup, SerdeJson>(&wallet_backup_file, None);

    let restored_wallet = Wallet::restore(
        &backup,
        &restored_wallet_file,
        &BackendConfig::CoreRpc(rpc_config.clone()),
        km.clone(),
    )
    .unwrap();

    assert_eq!(wallet, restored_wallet); // only compares .store!

    cleanup(&mut bitcoind, &root_dir);
}

/// Setup state for the Electrum-backed backup/restore tests.
struct ElectrumSetup {
    original_wallet: PathBuf,
    restored_wallet: PathBuf,
    backup_file: PathBuf,
    electrum_cfg: ElectrumConfig,
    bitcoind: BitcoinD,
    /// Owns the electrs child process for the lifetime of the test.
    electrsd: ElectrsD,
    root_dir: PathBuf,
}

fn setup_electrum(test_name: &str) -> ElectrumSetup {
    let root_dir = std::env::temp_dir().join(format!("coinswap-elec-{}", rand::random::<u64>()));
    let temp_dir = root_dir.join("wallet-tests").join(test_name);
    let wallets_dir = temp_dir.join("");
    let original_wallet_name = "original-wallet".to_string();
    let restored_wallet_name = "restored-wallet".to_string();

    if temp_dir.exists() {
        fs::remove_dir_all(&temp_dir).unwrap();
    }

    // bitcoind still mines and funds; electrs indexes for the wallet.
    let port_zmq = 28332 + rand::random::<u16>() % 1000;
    let zmq_addr = format!("tcp://127.0.0.1:{port_zmq}");
    let bitcoind = init_bitcoind(&temp_dir, zmq_addr);
    let electrsd = init_electrsd(&bitcoind, &temp_dir);
    let electrum_url = format!("tcp://{}", electrsd.electrum_url);
    std::thread::sleep(std::time::Duration::from_secs(2));
    let _ = electrsd.trigger();

    ElectrumSetup {
        original_wallet: wallets_dir.join(&original_wallet_name),
        restored_wallet: wallets_dir.join(&restored_wallet_name),
        backup_file: wallets_dir.join("wallet-backup.json"),
        electrum_cfg: ElectrumConfig { url: electrum_url },
        bitcoind,
        electrsd,
        root_dir,
    }
}

/// Wait until electrs has indexed up to bitcoind's tip.
///
/// electrs syncs asynchronously, so a wallet sync right after mining can read
/// a stale tip and cache UTXOs with outdated confirmation counts — which then
/// differs from a wallet synced after electrs caught up, failing the equality
/// assertion. `trigger()` (SIGUSR1) nudges electrs to sync on each poll.
fn wait_for_electrs_tip(bitcoind: &BitcoinD, electrsd: &ElectrsD, cfg: &ElectrumConfig) {
    let expected = bitcoind.client.get_block_count().unwrap();
    let probe = Electrum::new(cfg).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let _ = electrsd.trigger();
        if probe
            .get_block_count()
            .map(|tip| tip >= expected)
            .unwrap_or(false)
        {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "electrs did not reach tip {} within 60s",
            expected
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

#[test]
fn plainwallet_plainbackup_plainrestore_electrum() {
    info!("Running Test: Electrum-backed Wallet backup-restore");

    let mut s = setup_electrum("plain_wallet_plainbackup_plain_restore_electrum");

    let mut wallet = Wallet::init(
        &s.original_wallet,
        AnyBlockchain::Electrum(Electrum::new(&s.electrum_cfg).unwrap()),
        None,
    )
    .unwrap();

    let addr = wallet.get_next_external_address(AddressType::P2TR).unwrap();
    send_and_mine(&mut s.bitcoind, &addr, 0.05, 1).unwrap();
    wait_for_electrs_tip(&s.bitcoind, &s.electrsd, &s.electrum_cfg);

    wallet.backup(&s.backup_file, None).unwrap();

    let addr = wallet.get_next_external_address(AddressType::P2TR).unwrap();
    send_and_mine(&mut s.bitcoind, &addr, 0.05, 1).unwrap();
    wait_for_electrs_tip(&s.bitcoind, &s.electrsd, &s.electrum_cfg);

    wallet.sync_and_save().unwrap();

    let (backup, _) = load_sensitive_struct::<WalletBackup, SerdeJson>(&s.backup_file, None);

    let restored_wallet = Wallet::restore(
        &backup,
        &s.restored_wallet,
        &BackendConfig::Electrum(s.electrum_cfg.clone()),
        None,
    )
    .unwrap();

    assert_eq!(wallet, restored_wallet);

    cleanup(&mut s.bitcoind, &s.root_dir);

    info!("🎉 Electrum wallet backup-restore test ran successfully!");
}

#[test]
fn encwallet_encbackup_encrestore_electrum() {
    let mut s = setup_electrum("encwallet_encbackup_encrestore_electrum");

    let km = KeyMaterial::new_interactive(None);

    let mut wallet = Wallet::init(
        &s.original_wallet,
        AnyBlockchain::Electrum(Electrum::new(&s.electrum_cfg).unwrap()),
        km.clone(),
    )
    .unwrap();

    let addr = wallet.get_next_external_address(AddressType::P2TR).unwrap();
    send_and_mine(&mut s.bitcoind, &addr, 0.05, 1).unwrap();
    wait_for_electrs_tip(&s.bitcoind, &s.electrsd, &s.electrum_cfg);

    wallet.backup(&s.backup_file, km.clone()).unwrap();

    let addr = wallet.get_next_external_address(AddressType::P2TR).unwrap();
    send_and_mine(&mut s.bitcoind, &addr, 0.05, 1).unwrap();
    wait_for_electrs_tip(&s.bitcoind, &s.electrsd, &s.electrum_cfg);

    wallet.sync_and_save().unwrap();

    let (backup, _) = load_sensitive_struct::<WalletBackup, SerdeJson>(&s.backup_file, None);

    let restored_wallet = Wallet::restore(
        &backup,
        &s.restored_wallet,
        &BackendConfig::Electrum(s.electrum_cfg.clone()),
        km.clone(),
    )
    .unwrap();

    assert_eq!(wallet, restored_wallet);

    cleanup(&mut s.bitcoind, &s.root_dir);
}

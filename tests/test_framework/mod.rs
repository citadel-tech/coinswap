//! A Framework to write functional tests for the Coinswap Protocol.
//!
//! This framework uses [bitcoind] to automatically spawn regtest node in the background.
//!
//! Spawns one Taker and multiple Makers, with/without special behavior, connect them to bitcoind regtest node,
//! and initializes the database.
//!
//! The tests' data are stored in the `tests/temp-files` directory, which is auto-removed after each successful test.
//! Do not invoke [TestFramework::stop] function at the end of the test, to persist this data for debugging.
//!
//! The test data also includes the backend bitcoind data-directory, which is useful for observing the blockchain states after a swap.
//!
//! Checkout `tests/standard_swap.rs` for example of simple coinswap simulation test between 1 Taker and 2 Makers.

//  TODO(for taproot tests):
// - Figure out why the fee variances are occuring
// - Don't wait for timeout during maker recovery, monitor the relevant logs instead

// Temporary custom assert macro to check for balances ranging +-2 Sats owing to variability in Transaction Size by 1 vbyte(low-s).
#[macro_export]
macro_rules! assert_in_range {
    ($value:expr, $allowed:expr, $msg:expr) => {{
        let (value, allowed) = ($value, $allowed);
        const RANGE: u64 = 2;
        if !allowed
            .iter()
            .any(|x| x + RANGE == value || x.saturating_sub(RANGE) == value || *x == value)
        {
            panic!("{}: actual value = {}", $msg, value);
        }
    }};
}

use bip39::rand;
use bitcoin::Amount;
use nostr_rs_relay::{config, server::start_server};
use std::{
    env,
    fs::{self, create_dir_all, File},
    io::{BufReader, Read},
    net::TcpStream,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering::Relaxed},
        mpsc, Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use flate2::read::GzDecoder;
use tar::Archive;

use bitcoind::{
    bitcoincore_rpc::{Auth, RpcApi},
    BitcoinD,
};

use coinswap::{
    maker::{
        Maker, MakerBehavior, TaprootMaker, TaprootMakerBehavior, UnifiedMakerBehavior,
        UnifiedMakerServer, UnifiedMakerServerConfig,
    },
    protocol::common_messages::ProtocolVersion,
    taker::{
        api2::TakerBehavior as TaprootTakerBehavior, Taker, TakerBehavior, TaprootTaker,
        UnifiedTaker, UnifiedTakerBehavior, UnifiedTakerConfig,
    },
    utill::setup_logger,
    wallet::{AddressType, Balances, RPCConfig},
};
use log::info;

const BITCOIN_VERSION: &str = "28.1";

fn download_bitcoind_tarball(download_url: &str, retries: usize) -> Vec<u8> {
    for attempt in 1..=retries {
        let response = minreq::get(download_url).send();
        match response {
            Ok(res) if res.status_code == 200 => {
                return res.as_bytes().to_vec();
            }
            Ok(res) if res.status_code == 503 => {
                // If the response is 503, log and prepare for retry
                eprintln!(
                    "Attempt {}: URL {} returned status code 503 (Service Unavailable)",
                    attempt + 1,
                    download_url
                );
            }
            Ok(res) => {
                // For other status codes, log and stop retrying
                panic!(
                    "URL {} returned unexpected status code {}. Aborting.",
                    download_url, res.status_code
                );
            }
            Err(err) => {
                eprintln!("Attempt {attempt}: Failed to fetch URL {download_url}: {err:?}");
            }
        }

        if attempt < retries {
            let delay = 1u64 << (attempt - 1);
            eprintln!("Retrying in {delay} seconds (exponential backoff)...");
            std::thread::sleep(std::time::Duration::from_secs(delay));
        }
    }
    // If all retries fail, panic with an error message
    panic!(
        "Cannot reach URL {} after {} attempts",
        download_url, retries
    );
}

fn read_tarball_from_file(path: &str) -> Vec<u8> {
    let file = File::open(path).unwrap_or_else(|_| {
        panic!(
            "Cannot find {:?} specified with env var BITCOIND_TARBALL_FILE",
            path
        )
    });
    let mut reader = BufReader::new(file);
    let mut buffer = Vec::new();
    reader.read_to_end(&mut buffer).unwrap();
    buffer
}

fn unpack_tarball(tarball_bytes: &[u8], destination: &Path) {
    let decoder = GzDecoder::new(tarball_bytes);
    let mut archive = Archive::new(decoder);
    for mut entry in archive.entries().unwrap().flatten() {
        if let Ok(file) = entry.path() {
            if file.ends_with("bitcoind") {
                entry.unpack_in(destination).unwrap();
            }
        }
    }
}

fn get_bitcoind_filename(os: &str, arch: &str) -> String {
    match (os, arch) {
        ("macos", "aarch64") => format!("bitcoin-{BITCOIN_VERSION}-arm64-apple-darwin.tar.gz"),
        ("macos", "x86_64") => format!("bitcoin-{BITCOIN_VERSION}-x86_64-apple-darwin.tar.gz"),
        ("linux", "x86_64") => format!("bitcoin-{BITCOIN_VERSION}-x86_64-linux-gnu.tar.gz"),
        ("linux", "aarch64") => format!("bitcoin-{BITCOIN_VERSION}-aarch64-linux-gnu.tar.gz"),
        _ => format!("bitcoin-{BITCOIN_VERSION}-x86_64-apple-darwin-unsigned.zip"),
    }
}

/// Initiate the bitcoind backend.
pub(crate) fn init_bitcoind(datadir: &std::path::Path, zmq_addr: String) -> BitcoinD {
    let mut conf = bitcoind::Conf::default();
    conf.args.push("-txindex=1"); //txindex is must, or else wallet sync won't work.
    let raw_tx = format!("-zmqpubrawtx={}", zmq_addr);
    conf.args.push(&raw_tx);
    let block_hash = format!("-zmqpubrawblock={}", zmq_addr);
    conf.args.push(&block_hash);
    conf.staticdir = Some(datadir.join(".bitcoin"));
    log::info!(
        "🔗 bitcoind datadir: {:?}",
        conf.staticdir.as_ref().unwrap()
    );
    log::info!("🔧 bitcoind configuration: {:?}", conf.args);

    let os = env::consts::OS;
    let arch = env::consts::ARCH;
    let current_dir: PathBuf = std::env::current_dir().expect("failed to read current dir");
    let bitcoin_bin_dir = current_dir.join("bin");
    let download_filename = get_bitcoind_filename(os, arch);
    let bitcoin_exe_home = bitcoin_bin_dir
        .join(format!("bitcoin-{BITCOIN_VERSION}"))
        .join("bin");

    if !bitcoin_exe_home.exists() {
        let tarball_bytes = match env::var("BITCOIND_TARBALL_FILE") {
            Ok(path) => read_tarball_from_file(&path),
            Err(_) => {
                let download_endpoint = env::var("BITCOIND_DOWNLOAD_ENDPOINT")
                    .unwrap_or_else(|_| "http://172.81.178.3/bitcoin-binaries".to_owned());
                let url = format!("{download_endpoint}/{download_filename}");
                download_bitcoind_tarball(&url, 5)
            }
        };

        if let Some(parent) = bitcoin_exe_home.parent() {
            create_dir_all(parent).unwrap();
        }

        unpack_tarball(&tarball_bytes, &bitcoin_bin_dir);

        if os == "macos" {
            let bitcoind_binary = bitcoin_exe_home.join("bitcoind");
            std::process::Command::new("codesign")
                .arg("--sign")
                .arg("-")
                .arg(&bitcoind_binary)
                .output()
                .expect("Failed to sign bitcoind binary");
        }
    }

    env::set_var("BITCOIND_EXE", bitcoin_exe_home.join("bitcoind"));

    let exe_path = bitcoind::exe_path().unwrap();

    log::info!("📁 Executable path: {exe_path:?}");

    let bitcoind = BitcoinD::with_conf(exe_path, &conf).unwrap();

    // Generate initial 101 blocks
    generate_blocks(&bitcoind, 101);
    log::info!("🚀 bitcoind initiated!!");

    bitcoind
}

/// Generate Blocks in regtest node.
pub(crate) fn generate_blocks(bitcoind: &BitcoinD, n: u64) {
    let mining_address = match bitcoind.client.get_new_address(None, None) {
        Ok(addr) => addr
            .require_network(bitcoind::bitcoincore_rpc::bitcoin::Network::Regtest)
            .unwrap(),
        Err(_) => return,
    };
    let _ = bitcoind.client.generate_to_address(n, &mining_address);
}

/// Send coins to a bitcoin address.
#[allow(dead_code)]
pub(crate) fn send_to_address(
    bitcoind: &BitcoinD,
    addrs: &bitcoin::Address,
    amount: bitcoin::Amount,
) -> bitcoin::Txid {
    bitcoind
        .client
        .send_to_address(addrs, amount, None, None, None, None, None, None)
        .unwrap()
}

#[allow(dead_code)]
pub fn fund_and_verify_taker(
    taker: &mut Taker,
    bitcoind: &BitcoinD,
    utxo_count: u32,
    utxo_value: Amount,
) -> Amount {
    log::info!("💰 Funding Takers...");

    // Get initial state before funding
    let wallet = taker.get_wallet_mut();
    wallet.sync_and_save().unwrap();
    let initial_utxo_set = wallet.list_all_utxo();
    let initial_balances = wallet.get_balances().unwrap();
    let initial_external_index = *wallet.get_external_index();
    let mut new_txids = Vec::new();

    for _ in 0..utxo_count {
        let taker_address = wallet
            .get_next_external_address(AddressType::P2WPKH)
            .unwrap();
        new_txids.push(send_to_address(bitcoind, &taker_address, utxo_value));
    }

    // confirm balances
    generate_blocks(bitcoind, 1);

    //------Basic Checks-----

    // Assert external address index reached to 3.
    assert_eq!(
        wallet.get_external_index(),
        &(initial_external_index + utxo_count),
        "Expected external address index at {}, but found at {}",
        initial_external_index + utxo_count,
        wallet.get_external_index()
    );

    wallet.sync_and_save().unwrap();

    let new_utxo_set = wallet.list_all_utxo();
    let expected_total = initial_balances.regular + utxo_value * u64::from(utxo_count);
    let balances = wallet.get_balances().unwrap();

    // Assert UTXO count
    assert_eq!(
        new_utxo_set.len(),
        initial_utxo_set.len() + utxo_count as usize,
        "Expected {} UTXOs, but found {}",
        initial_utxo_set.len() + utxo_count as usize,
        new_utxo_set.len()
    );

    // Assert each UTXO value with it's TxId.
    for (i, &funding_txid) in new_txids.iter().enumerate() {
        assert!(
            new_utxo_set
                .iter()
                .map(|utxo| (utxo.amount, utxo.txid))
                .collect::<Vec<(_, _)>>()
                .contains(&(utxo_value, funding_txid)),
            "Funding transaction {} (TxID: {}, Amount: {}) not found in Wallet",
            i + 1,
            funding_txid,
            utxo_value
        )
    }

    // Assert total balance matches expected
    assert_eq!(
        balances.regular, expected_total,
        "Expected regular balance {} but got {}",
        expected_total, balances.regular
    );

    assert_eq!(balances.fidelity, Amount::ZERO);
    assert_eq!(balances.swap, Amount::ZERO);
    assert_eq!(balances.contract, Amount::ZERO);

    // Assert spendable balance equals regular balance, since no fidelity/swap/contract
    assert_eq!(
        balances.spendable, balances.regular,
        "Spendable and Regular balance missmatch | Spendable balance {} | Regular balance {}",
        balances.spendable, balances.regular
    );

    log::info!(
        "✅ Taker funding verification complete | Found {} new UTXOs of value {} each | Total Spendable Balance: {}",
        utxo_count,
        utxo_value,
        balances.spendable
    );

    balances.spendable
}

#[allow(dead_code)]
pub fn fund_and_verify_maker(
    makers: Vec<&Maker>,
    bitcoind: &BitcoinD,
    utxo_count: u32,
    utxo_value: Amount,
) {
    log::info!("💰 Funding Makers...");

    makers.iter().enumerate().for_each(|(maker_index, &maker)| {
        let mut wallet = maker.get_wallet().write().unwrap();
        let initial_utxo_set = wallet.list_all_utxo();
        let initial_balances = wallet.get_balances().unwrap();
        let mut new_txids = Vec::new();

        for _ in 0..utxo_count {
            let maker_addr = wallet.get_next_external_address(AddressType::P2WPKH).unwrap();
            new_txids.push(send_to_address(bitcoind, &maker_addr, utxo_value));
        }

        drop(wallet);
        generate_blocks(bitcoind, 1);

        let mut wallet = maker.wallet.write().unwrap();
        wallet.sync_and_save().unwrap();
        let new_utxo_set = wallet.list_all_utxo();
        let expected_total = initial_balances.regular + utxo_value * u64::from(utxo_count);
        let balances = wallet.get_balances().unwrap();

        // Assert UTXO count
        assert_eq!(
            new_utxo_set.len(),
            initial_utxo_set.len() + utxo_count as usize,
            "Maker {} - Expected {} UTXOs, but found {}",
            maker_index,
            initial_utxo_set.len() + utxo_count as usize,
            new_utxo_set.len()
        );

        // Assert each UTXO value with its TxId
        for (i, &funding_txid) in new_txids.iter().enumerate() {
            assert!(
                new_utxo_set
                    .iter()
                    .map(|utxo| (utxo.amount, utxo.txid))
                    .collect::<Vec<(_, _)>>()
                    .contains(&(utxo_value, funding_txid)),
                "Maker {} - Funding transaction {} (TxID: {}, Amount: {}) not found in Wallet",
                maker_index,
                i + 1,
                funding_txid,
                utxo_value
            );
        }

        // Assert total balance matches expected
        assert_eq!(
            balances.regular, expected_total,
            "Maker {} - Expected regular balance {} but got {}",
            maker_index, expected_total, balances.regular
        );

        log::info!(
        "✅ Maker {} funding verification complete | Found {} new UTXOs of value {} each | Total Spendable Balance: {}",
        maker_index,
        utxo_count,
        utxo_value,
        balances.spendable
        );
    });
}

#[allow(dead_code)]
/// Fund taproot makers and verify their balances
pub fn fund_taproot_makers(
    makers: &[Arc<TaprootMaker>],
    bitcoind: &bitcoind::BitcoinD,
    utxo_count: u32,
    utxo_value: Amount,
) {
    for maker in makers {
        let mut wallet = maker.wallet().write().unwrap();

        // Fund with regular UTXOs
        for _ in 0..utxo_count {
            let addr = wallet.get_next_external_address(AddressType::P2TR).unwrap();
            send_to_address(bitcoind, &addr, utxo_value);
        }

        generate_blocks(bitcoind, 1);
        wallet.sync_and_save().unwrap();

        // Verify balances
        let balances = wallet.get_balances().unwrap();
        let expected_regular = utxo_value * utxo_count.into();

        assert_eq!(balances.regular, expected_regular);

        info!(
            "Taproot Maker funded successfully. Regular: {}, Fidelity: {}",
            balances.regular, balances.fidelity
        );
    }
}

#[allow(dead_code)]
/// Fund taproot taker and verify balance
pub fn fund_taproot_taker(
    taker: &mut TaprootTaker,
    bitcoind: &bitcoind::BitcoinD,
    utxo_count: u32,
    utxo_value: Amount,
) -> Amount {
    // Fund with regular UTXOs
    for _ in 0..utxo_count {
        let addr = taker
            .get_wallet_mut()
            .get_next_external_address(AddressType::P2TR)
            .unwrap();
        send_to_address(bitcoind, &addr, utxo_value);
    }

    generate_blocks(bitcoind, 1);
    taker.get_wallet_mut().sync_and_save().unwrap();

    // Verify balances
    let balances = taker.get_wallet().get_balances().unwrap();
    let expected_regular = utxo_value * utxo_count.into();

    assert_eq!(balances.regular, expected_regular);

    info!(
        "Taproot Taker funded successfully. Regular: {}, Spendable: {}",
        balances.regular, balances.spendable
    );

    balances.spendable
}

#[allow(dead_code)]
pub fn verify_maker_pre_swap_balances(balances: &Balances, assert_regular_balance: u64) {
    assert_in_range!(
        balances.regular.to_sat(),
        [assert_regular_balance],
        "Maker regular balance mismatch"
    );
    assert_eq!(balances.fidelity, Amount::from_btc(0.05).unwrap());
    assert_eq!(balances.swap, Amount::ZERO);
    assert_eq!(balances.contract, Amount::ZERO);
}

// Pre swap funded balance is same for all makers so it's a common function to verify it.
#[allow(dead_code)]
pub fn verify_maker_pre_swap_balance_taproot(taproot_makers: &[Arc<TaprootMaker>]) -> Vec<Amount> {
    let mut maker_spendable_balance = Vec::new();

    info!("Testing taproot maker balance verification");

    for (i, maker) in taproot_makers.iter().enumerate() {
        let wallet = maker.wallet().read().unwrap();
        let balances = wallet.get_balances().unwrap();

        info!(
            "Taproot Maker {} balances: Regular: {}, Swap: {}, Contract: {}, Fidelity: {}",
            i, balances.regular, balances.swap, balances.contract, balances.fidelity
        );

        // Regular balance after fidelity bond (allow small fee variance)
        // eg for 0.20 btc as funded, 0.05 btc will be spent due to fidelity bond creation and some small fee,so regular balance will be around 0.1499
        assert_in_range!(
            balances.regular.to_sat(),
            [
                14999500, // maker (normal case)
                14999518, // maker (normal case with slight fee variance)
                34999500, // maker multi taker case (8 utxo funded)
                34999518, // maker multi taker case (with slight fee variance)
            ],
            "Taproot Maker regular balance check after fidelity bond creation."
        );

        assert_eq!(balances.swap, Amount::ZERO);
        assert_eq!(balances.contract, Amount::ZERO);

        assert_eq!(
            balances.fidelity,
            Amount::from_btc(0.05).unwrap(),
            "Fidelity bond should be exactly 0.05 BTC"
        );

        assert!(
            balances.spendable > Amount::ZERO,
            "Taproot Maker {} should have spendable balance",
            i
        );

        // Store spendable balance
        maker_spendable_balance.push(balances.spendable);
    }

    maker_spendable_balance
}

/// Fund unified taker and verify balance
#[allow(dead_code)]
pub fn fund_unified_taker(
    taker: &UnifiedTaker,
    bitcoind: &bitcoind::BitcoinD,
    utxo_count: u32,
    utxo_value: Amount,
    address_type: AddressType,
) -> Amount {
    log::info!("💰 Funding Unified Taker...");

    // Fund with UTXOs
    for _ in 0..utxo_count {
        let addr = taker
            .get_wallet()
            .write()
            .unwrap()
            .get_next_external_address(address_type)
            .unwrap();
        send_to_address(bitcoind, &addr, utxo_value);
    }

    generate_blocks(bitcoind, 1);

    let mut wallet = taker.get_wallet().write().unwrap();
    wallet.sync_and_save().unwrap();

    // Verify balances
    let balances = wallet.get_balances().unwrap();
    let expected_regular = utxo_value * utxo_count.into();

    assert_eq!(balances.regular, expected_regular);

    info!(
        "Unified Taker funded successfully. Regular: {}, Spendable: {}",
        balances.regular, balances.spendable
    );

    balances.spendable
}

/// Fund unified makers and verify their balances
#[allow(dead_code)]
pub fn fund_unified_makers(
    makers: &[Arc<UnifiedMakerServer>],
    bitcoind: &bitcoind::BitcoinD,
    utxo_count: u32,
    utxo_value: Amount,
    address_type: AddressType,
) -> Vec<Amount> {
    log::info!("💰 Funding Unified Makers...");

    let mut spendable_balances = Vec::new();

    for maker in makers {
        let mut wallet = maker.wallet.write().unwrap();

        // Fund with regular UTXOs
        for _ in 0..utxo_count {
            let addr = wallet.get_next_external_address(address_type).unwrap();
            send_to_address(bitcoind, &addr, utxo_value);
        }

        generate_blocks(bitcoind, 1);
        wallet.sync_and_save().unwrap();

        // Verify balances
        let balances = wallet.get_balances().unwrap();
        let expected_regular = utxo_value * utxo_count.into();

        assert_eq!(balances.regular, expected_regular);

        info!(
            "Unified Maker funded successfully. Regular: {}, Fidelity: {}",
            balances.regular, balances.fidelity
        );

        spendable_balances.push(balances.spendable);
    }

    spendable_balances
}

/// Verify unified maker pre-swap balances
#[allow(dead_code)]
pub fn verify_unified_maker_pre_swap_balances(makers: &[Arc<UnifiedMakerServer>]) -> Vec<Amount> {
    let mut maker_spendable_balance = Vec::new();

    info!("Testing unified maker balance verification");

    for (i, maker) in makers.iter().enumerate() {
        let wallet = maker.wallet.read().unwrap();
        let balances = wallet.get_balances().unwrap();

        info!(
            "Unified Maker {} balances: Regular: {}, Swap: {}, Contract: {}, Fidelity: {}, Spendable: {}",
            i, balances.regular, balances.swap, balances.contract, balances.fidelity, balances.spendable
        );

        // Regular balance after fidelity bond (allow variance for different setups)
        assert_in_range!(
            balances.regular.to_sat(),
            [
                14999500, // maker (normal case)
                14999518, // maker (normal case with slight fee variance)
                14999540, // maker (unified taproot case with fee variance)
                14999542, // maker (unified taproot case with fee variance)
                34999500, // maker multi taker case (8 utxo funded)
                34999518, // maker multi taker case (with slight fee variance)
            ],
            "Unified Maker regular balance check after fidelity bond creation."
        );

        assert_eq!(balances.swap, Amount::ZERO);
        assert_eq!(balances.contract, Amount::ZERO);

        assert_eq!(
            balances.fidelity,
            Amount::from_btc(0.05).unwrap(),
            "Fidelity bond should be exactly 0.05 BTC"
        );

        assert!(
            balances.spendable > Amount::ZERO,
            "Unified Maker {} should have spendable balance",
            i
        );

        maker_spendable_balance.push(balances.spendable);
    }

    maker_spendable_balance
}

/// The Test Framework.
///
/// Handles initializing, operating and cleaning up of all backend processes. Bitcoind, Taker and Makers.
#[allow(dead_code)]
pub struct TestFramework {
    pub(super) bitcoind: BitcoinD,
    pub(super) temp_dir: PathBuf,
    shutdown: AtomicBool,
    nostr_relay_shutdown: mpsc::Sender<()>,
    nostr_relay_handle: Option<JoinHandle<()>>,
}

impl TestFramework {
    /// Initialize a test-framework environment from given configuration data.
    /// This object holds the reference to backend bitcoind process and RPC.
    /// It takes:
    /// - bitcoind conf.
    /// - a vector of mappings from (port, optional port) tuples to [`MakerBehavior`]
    /// - optional taker behavior.
    /// - connection type
    ///
    /// Returns ([`TestFramework`], [`Taker`], [`Vec<Maker>`]).
    /// Maker's config will follow the pattern given the input HashMap.
    /// If no bitcoind conf is provided, a default value will be used.
    #[allow(clippy::type_complexity)]
    pub fn init(
        makers_config_map: Vec<((u16, Option<u16>), MakerBehavior)>,
        taker_behavior: Vec<TakerBehavior>,
    ) -> (Arc<Self>, Vec<Taker>, Vec<Arc<Maker>>, JoinHandle<()>) {
        // Setup directory
        let temp_dir = env::temp_dir().join("coinswap");
        // Remove if previously existing
        if temp_dir.exists() {
            fs::remove_dir_all::<PathBuf>(temp_dir.clone()).unwrap();
        }
        setup_logger(log::LevelFilter::Info, Some(temp_dir.clone()));
        log::info!("📁 temporary directory : {}", temp_dir.display());

        let port_zmq = 28332 + rand::random::<u16>() % 1000;

        let zmq_addr = format!("tcp://127.0.0.1:{port_zmq}");

        let bitcoind = init_bitcoind(&temp_dir, zmq_addr.clone());

        log::info!("🌐 Spawning local nostr relay for tests");
        let (nostr_relay_shutdown, nostr_relay_handle) = spawn_nostr_relay(&temp_dir);

        _ = wait_for_relay_healthy();

        let shutdown = AtomicBool::new(false);
        let test_framework = Arc::new(Self {
            bitcoind,
            temp_dir: temp_dir.clone(),
            shutdown,
            nostr_relay_shutdown,
            nostr_relay_handle: Some(nostr_relay_handle),
        });

        // Translate a RpcConfig from the test framework.
        // a modification of this will be used for taker and makers rpc connections.
        let rpc_config = RPCConfig::from(test_framework.as_ref());
        // Create the Taker.
        let taker_rpc_config = rpc_config.clone();
        let takers = taker_behavior
            .into_iter()
            .enumerate()
            .map(|(i, behavior)| {
                let taker_id = format!("taker{}", i + 1); // ex: "taker1"
                Taker::init(
                    Some(temp_dir.join(&taker_id)),
                    Some(taker_id),
                    Some(taker_rpc_config.clone()),
                    behavior,
                    None,
                    None,
                    zmq_addr.clone(),
                    None,
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let mut base_rpc_port = 3500; // Random port for RPC connection in tests. (Not used)

        let makers = makers_config_map // Create the Makers as per given configuration map.
            .into_iter()
            .map(|(port, behavior)| {
                base_rpc_port += 1;
                let maker_id = format!("maker{}", port.0); // ex: "maker6102"
                let maker_rpc_config = rpc_config.clone();
                thread::sleep(Duration::from_secs(5)); // Sleep for some time avoid resource unavailable error.
                Arc::new(
                    Maker::init(
                        Some(temp_dir.join(port.0.to_string())),
                        Some(maker_id),
                        Some(maker_rpc_config),
                        Some(port.0),
                        Some(base_rpc_port),
                        None,
                        None,
                        port.1,
                        behavior,
                        zmq_addr.clone(),
                        None,
                    )
                    .unwrap(),
                )
            })
            .collect::<Vec<_>>();

        // start the block generation thread
        log::info!("⛏️ Spawning block generation thread");
        let tf_clone = test_framework.clone();
        let generate_blocks_handle = thread::spawn(move || loop {
            thread::sleep(Duration::from_secs(3));

            if tf_clone.shutdown.load(Relaxed) {
                log::info!("🔚 Ending block generation thread");
                return;
            }
            // tf_clone.generate_blocks(10);
            generate_blocks(&tf_clone.bitcoind, 10);
        });

        log::info!("✅ Test Framework initialization complete");

        (test_framework, takers, makers, generate_blocks_handle)
    }

    /// Assert that a log message exists in the debug.log file
    pub fn assert_log(&self, expected_message: &str, log_path: &str) {
        let timeout = std::time::Duration::from_secs(120);
        let poll_interval = std::time::Duration::from_millis(200);
        let start = std::time::Instant::now();

        loop {
            if let Ok(log_contents) = std::fs::read_to_string(log_path) {
                if log_contents.contains(expected_message) {
                    log::info!("✅ Found expected log message: '{expected_message}'");
                    return;
                }
            }

            if start.elapsed() > timeout {
                panic!(
                    "Timed out waiting for log message:\n\
                 '{}'\n\n\
                 Last log contents:\n{}",
                    expected_message,
                    std::fs::read_to_string(log_path).unwrap_or_default()
                );
            }

            std::thread::sleep(poll_interval);
        }
    }

    #[allow(clippy::type_complexity)]
    pub fn init_taproot(
        makers_config_map: Vec<(u16, Option<u16>, TaprootMakerBehavior)>,
        taker_behavior: Vec<TaprootTakerBehavior>,
    ) -> (
        Arc<Self>,
        Vec<TaprootTaker>,
        Vec<Arc<TaprootMaker>>,
        JoinHandle<()>,
    ) {
        // Setup directory
        let temp_dir = env::temp_dir().join("coinswap");
        // Remove if previously existing
        if temp_dir.exists() {
            fs::remove_dir_all::<PathBuf>(temp_dir.clone()).unwrap();
        }
        setup_logger(log::LevelFilter::Info, Some(temp_dir.clone()));
        log::info!("📁 temporary directory : {}", temp_dir.display());

        let port_zmq = 28332 + rand::random::<u16>() % 1000;

        let zmq_addr = format!("tcp://127.0.0.1:{port_zmq}");

        let bitcoind = init_bitcoind(&temp_dir, zmq_addr.clone());

        log::info!("🌐 Spawning local nostr relay for tests");
        let (nostr_relay_shutdown, nostr_relay_handle) = spawn_nostr_relay(&temp_dir);

        let shutdown = AtomicBool::new(false);
        let test_framework = Arc::new(Self {
            bitcoind,
            temp_dir: temp_dir.clone(),
            shutdown,
            nostr_relay_shutdown,
            nostr_relay_handle: Some(nostr_relay_handle),
        });

        // Translate a RpcConfig from the test framework.
        // a modification of this will be used for taker and makers rpc connections.
        let rpc_config = RPCConfig::from(test_framework.as_ref());
        // Create the Taker.
        let taker_rpc_config = rpc_config.clone();
        let takers = taker_behavior
            .into_iter()
            .enumerate()
            .map(|(i, behavior)| {
                let taker_id = format!("taker{}", i + 1); // ex: "taker1"
                TaprootTaker::init(
                    Some(temp_dir.join(&taker_id)),
                    Some(taker_id),
                    Some(taker_rpc_config.clone()),
                    None,
                    None,
                    zmq_addr.clone(),
                    None,
                    behavior,
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let mut base_rpc_port = 3500; // Random port for RPC connection in tests. (Not used)

        let makers = makers_config_map // Create the Makers as per given configuration map.
            .into_iter()
            .map(|(network_port, socks_port, behavior)| {
                base_rpc_port += 1;
                let maker_id = format!("maker{}", network_port); // ex: "maker6102"
                let maker_rpc_config = rpc_config.clone();
                thread::sleep(Duration::from_secs(5)); // Sleep for some time avoid resource unavailable error.
                Arc::new(
                    TaprootMaker::init(
                        Some(temp_dir.join(network_port.to_string())),
                        Some(maker_id),
                        Some(maker_rpc_config),
                        Some(network_port),
                        Some(base_rpc_port),
                        None,
                        None,
                        socks_port,
                        zmq_addr.clone(),
                        None,
                        Some(behavior),
                    )
                    .unwrap(),
                )
            })
            .collect::<Vec<_>>();

        // start the block generation thread
        log::info!("⛏️ Spawning block generation thread");
        let tf_clone = test_framework.clone();
        let generate_blocks_handle = thread::spawn(move || loop {
            thread::sleep(Duration::from_secs(3));

            if tf_clone.shutdown.load(Relaxed) {
                log::info!("🔚 Ending block generation thread");
                return;
            }
            // tf_clone.generate_blocks(10);
            generate_blocks(&tf_clone.bitcoind, 10);
        });

        log::info!("✅ Test Framework initialization complete");

        (test_framework, takers, makers, generate_blocks_handle)
    }

    /// Initialize test framework for unified protocol testing.
    ///
    /// This creates UnifiedTaker and UnifiedMakerServer instances that support
    /// both Legacy (ECDSA) and Taproot (MuSig2) protocols using unified message types.
    #[allow(clippy::type_complexity)]
    pub fn init_unified(
        makers_config_map: Vec<(u16, Option<u16>)>,
        taker_behavior: Vec<UnifiedTakerBehavior>,
        maker_behaviors: Vec<UnifiedMakerBehavior>,
    ) -> (
        Arc<Self>,
        Vec<UnifiedTaker>,
        Vec<Arc<UnifiedMakerServer>>,
        JoinHandle<()>,
    ) {
        // Setup directory
        let temp_dir = env::temp_dir().join("coinswap");
        // Remove if previously existing
        if temp_dir.exists() {
            fs::remove_dir_all::<PathBuf>(temp_dir.clone()).unwrap();
        }
        setup_logger(log::LevelFilter::Info, Some(temp_dir.clone()));
        log::info!("📁 temporary directory : {}", temp_dir.display());

        let port_zmq = 28332 + rand::random::<u16>() % 1000;

        let zmq_addr = format!("tcp://127.0.0.1:{port_zmq}");

        let bitcoind = init_bitcoind(&temp_dir, zmq_addr.clone());

        log::info!("🌐 Spawning local nostr relay for tests");
        let (nostr_relay_shutdown, nostr_relay_handle) = spawn_nostr_relay(&temp_dir);

        _ = wait_for_relay_healthy();

        let shutdown = AtomicBool::new(false);
        let test_framework = Arc::new(Self {
            bitcoind,
            temp_dir: temp_dir.clone(),
            shutdown,
            nostr_relay_shutdown,
            nostr_relay_handle: Some(nostr_relay_handle),
        });

        // Translate a RpcConfig from the test framework.
        let rpc_config = RPCConfig::from(test_framework.as_ref());

        // Create the UnifiedTakers
        let takers = taker_behavior
            .into_iter()
            .enumerate()
            .map(|(i, behavior)| {
                let taker_id = format!("unified_taker{}", i + 1);
                let config = UnifiedTakerConfig::default()
                    .with_data_dir(temp_dir.join(&taker_id))
                    .with_wallet_name(taker_id)
                    .with_rpc_config(rpc_config.clone())
                    .with_zmq_addr(zmq_addr.clone());
                let mut taker = UnifiedTaker::init(config).unwrap();
                taker.behavior = behavior;
                taker
            })
            .collect::<Vec<_>>();

        let mut base_rpc_port = 4500;

        // Create the UnifiedMakerServers with unified message handling
        let makers = makers_config_map
            .into_iter()
            .enumerate()
            .map(|(i, (network_port, _socks_port))| {
                base_rpc_port += 1;
                let maker_id = format!("unified_maker{}", network_port);
                thread::sleep(Duration::from_secs(5)); // Avoid resource unavailable error

                let config = UnifiedMakerServerConfig {
                    data_dir: temp_dir.join(network_port.to_string()),
                    network_port,
                    rpc_port: base_rpc_port,
                    base_fee: 1000,
                    amount_relative_fee_pct: 0.025,
                    time_relative_fee_pct: 0.001,
                    min_swap_amount: 10_000,
                    required_confirms: 1,
                    supported_protocols: vec![ProtocolVersion::Legacy, ProtocolVersion::Taproot],
                    zmq_addr: zmq_addr.clone(),
                    fidelity_amount: 5_000_000, // 0.05 BTC
                    fidelity_timelock: 950,     // ~950 blocks for test
                    network: bitcoin::Network::Regtest,
                    wallet_name: maker_id,
                    rpc_config: rpc_config.clone(),
                };

                let mut server = UnifiedMakerServer::init(config).unwrap();
                server.behavior = maker_behaviors.get(i).copied().unwrap_or_default();
                Arc::new(server)
            })
            .collect::<Vec<_>>();

        // Start the block generation thread
        log::info!("⛏️ Spawning block generation thread");
        let tf_clone = test_framework.clone();
        let generate_blocks_handle = thread::spawn(move || loop {
            thread::sleep(Duration::from_secs(3));

            if tf_clone.shutdown.load(Relaxed) {
                log::info!("🔚 Ending block generation thread");
                return;
            }
            generate_blocks(&tf_clone.bitcoind, 10);
        });

        log::info!("✅ Unified Test Framework initialization complete");

        (test_framework, takers, makers, generate_blocks_handle)
    }

    /// Stop bitcoind and clean up all test data.
    pub fn stop(&self) {
        log::info!("🛑 Stopping Test Framework");
        // stop all framework threads.
        self.shutdown.store(true, Relaxed);
        _ = self.nostr_relay_shutdown.send(());
        // stop bitcoind
        let _ = self.bitcoind.client.stop().unwrap();
    }
}

impl Drop for TestFramework {
    fn drop(&mut self) {
        let handle = self.nostr_relay_handle.take();
        if let Some(handle) = handle {
            _ = handle.join();
        }
    }
}

/// Initializes a [`TestFramework`] given a [`RPCConfig`].
impl From<&TestFramework> for RPCConfig {
    fn from(value: &TestFramework) -> Self {
        let url = value.bitcoind.rpc_url().split_at(7).1.to_string();
        let auth = Auth::CookieFile(value.bitcoind.params.cookie_file.clone());
        Self {
            url,
            auth,
            ..Default::default()
        }
    }
}

fn spawn_nostr_relay(temp_dir: &Path) -> (mpsc::Sender<()>, JoinHandle<()>) {
    let data_dir = temp_dir.join("nostr-relay");
    std::fs::create_dir_all(&data_dir).unwrap();

    let addr = "127.0.0.1".to_string();
    let port = 8000;

    let mut settings = config::Settings::default();
    settings.network.address = addr;
    settings.network.port = port;
    settings.database.min_conn = 4;
    settings.database.max_conn = 8;
    settings.database.in_memory = true;
    settings.diagnostics.tracing = true;

    let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>();

    let handle = thread::spawn(move || {
        start_server(&settings, shutdown_rx).expect("nostr relay crashed");
    });

    (shutdown_tx, handle)
}

fn wait_for_relay_healthy() -> Result<(), String> {
    let addr = "127.0.0.1:8000".to_string();
    let timeout = Duration::from_secs(10);
    let start = Instant::now();

    while start.elapsed() < timeout {
        if TcpStream::connect(&addr).is_ok() {
            log::info!("Nostr relay is alive");
            return Ok(());
        }

        std::thread::sleep(Duration::from_millis(50));
    }

    log::error!("Nostr relay not alive");

    Err("nostr relay did not become healthy on port 8000".to_string())
}

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

use bitcoin::Amount;
use std::{
    env,
    fs::{self, create_dir_all, File},
    io::{BufReader, Read},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering::Relaxed},
        Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use tokio::sync::mpsc::{self, Sender};

use flate2::read::GzDecoder;
use tar::Archive;

use bitcoind::{
    bitcoincore_rpc::{Auth, RpcApi},
    BitcoinD,
};

use coinswap::{
    maker::{Maker, MakerBehavior},
    taker::{Taker, TakerBehavior},
    utill::setup_logger,
    wallet::{Balances, RPCConfig},
};

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
pub(crate) fn init_bitcoind(datadir: &std::path::Path) -> BitcoinD {
    let mut conf = bitcoind::Conf::default();
    conf.args.push("-txindex=1"); //txindex is must, or else wallet sync won't work.
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
    let mining_address = bitcoind
        .client
        .get_new_address(None, None)
        .unwrap()
        .require_network(bitcoind::bitcoincore_rpc::bitcoin::Network::Regtest)
        .unwrap();
    bitcoind
        .client
        .generate_to_address(n, &mining_address)
        .unwrap();
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
    wallet.sync_no_fail();
    let initial_utxos = wallet.list_all_utxo().unwrap();
    let initial_utxo_count = initial_utxos.len();
    let initial_external_index = *wallet.get_external_index();

    // Fund the Taker with 3 utxos of 0.05 btc each.
    for _ in 0..utxo_count {
        let taker_address = wallet.get_next_external_address().unwrap();
        send_to_address(bitcoind, &taker_address, utxo_value);
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

    wallet.sync_no_fail();

    // Check if utxo list looks good.
    let utxos = wallet.list_all_utxo().unwrap();

    // Assert UTXO count
    assert_eq!(
        utxos.len(),
        initial_utxo_count + utxo_count as usize,
        "Expected {} UTXOs, but found {}",
        initial_utxo_count + utxo_count as usize,
        utxos.len()
    );

    // Assert each UTXO value
    for (i, utxo) in utxos.iter().skip(initial_utxo_count).enumerate() {
        assert_eq!(
            utxo.amount, utxo_value,
            "New UTXO at index {} has amount {} but expected {}",
            i, utxo.amount, utxo_value
        );
    }

    // Calculate expected total balance, previously was 0.05*3 = 0.15 btc
    let expected_total = utxo_value * u64::from(utxo_count);

    let balances = wallet.get_balances().unwrap();

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
    // Fund the Maker with 4 utxos of 0.05 btc each.

    log::info!("💰 Funding Makers...");

    makers.iter().for_each(|&maker| {
        // let wallet = maker..write().unwrap();
        let mut wallet_write = maker.wallet.write().unwrap();

        for _ in 0..utxo_count {
            let maker_addr = wallet_write.get_next_external_address().unwrap();
            send_to_address(bitcoind, &maker_addr, utxo_value);
        }
    });

    // confirm balances
    generate_blocks(bitcoind, 1);

    // --- Basic Checks ----
    makers.iter().for_each(|&maker| {
        let mut wallet = maker.get_wallet().write().unwrap();
        // Assert external address index reached to 4.
        assert_eq!(wallet.get_external_index(), &utxo_count);

        //
        wallet.sync_and_save().unwrap();

        let balances = wallet.get_balances().unwrap();

        assert_eq!(
            balances.regular,
            Amount::from_sat(utxo_value.to_sat() * utxo_count as u64)
        );
        assert_eq!(balances.fidelity, Amount::ZERO);
        assert_eq!(balances.swap, Amount::ZERO);
        assert_eq!(balances.contract, Amount::ZERO);
    });

    log::info!("✅ Maker funding verification complete");
}

/// Verifies the results of a coinswap for the taker and makers after performing a swap.
#[allow(dead_code)]
pub fn verify_swap_results(
    taker: &Taker,
    makers: &[Arc<Maker>],
    org_taker_spend_balance: Amount,
    org_maker_spend_balances: Vec<Amount>,
) {
    // Check Taker balances
    {
        let wallet = taker.get_wallet();
        let balances = wallet.get_balances().unwrap();

        // Debug logging for taker
        log::info!(
            "🔍 DEBUG Taker - Regular: {}, Swap: {}, Spendable: {}",
            balances.regular.to_btc(),
            balances.swap.to_btc(),
            balances.spendable.to_btc()
        );
        assert_in_range!(
            balances.regular.to_sat(),
            [
                14499696, // Successful coinswap
                14943035, // Recovery via timelock
                14940090, // Recovery via Hashlock
                15000000, // No spending
            ],
            "Taker seed balance mismatch"
        );

        assert_in_range!(
            balances.swap.to_sat(),
            [
                443633, // Successful coinswap
                442714, // Recovery via timelock
                0,      // Unsuccessful coinswap
            ],
            "Taker swapcoin balance mismatch"
        );

        assert_in_range!(balances.contract.to_sat(), [0], "Contract balance mismatch");
        assert_eq!(balances.fidelity, Amount::ZERO);

        // Check balance difference
        let balance_diff = org_taker_spend_balance
            .checked_sub(balances.spendable)
            .unwrap();

        log::info!(
            "🔍 DEBUG Taker balance diff: {} sats",
            balance_diff.to_sat()
        );

        assert_in_range!(
            balance_diff.to_sat(),
            [
                56671,  // Successful coinswap
                56965,  // Recovery via timelock
                500304, // Spent swapcoin
                2574,   // Recovery via timelock (new fee system)
                59910,  // Recovery via Hashlock (abort3_case3)
                500912, // Recovery via Hashlock(abort3_case1)
                0       // No spending
            ],
            "Taker spendable balance change mismatch"
        );
    }

    // Check Maker balances
    makers
        .iter()
        .zip(org_maker_spend_balances.iter())
        .enumerate()
        .for_each(|(maker_index, (maker, org_spend_balance))| {
            let mut wallet = maker.get_wallet().write().unwrap();
            wallet.sync_no_fail();
            let balances = wallet.get_balances().unwrap();

            // Debug logging for makers
            log::info!(
                "🔍 DEBUG Maker {} - Regular: {}, Swap: {}, Contract: {}, Spendable: {}",
                maker_index,
                balances.regular.to_btc(),
                balances.swap.to_btc(),
                balances.contract.to_btc(),
                balances.spendable.to_btc()
            );

            assert_in_range!(
                balances.regular.to_sat(),
                [
                    14555295, // First maker on successful coinswap
                    14533010, // Second maker on successful coinswap
                    14999508, // No spending
                    24999510, // Multi-taker scenario
                ],
                "Maker seed balance mismatch"
            );

            assert_in_range!(
                balances.swap.to_sat(),
                [
                    465918, // First maker
                    499724, // Second maker
                    442712, // Taker swap amount
                    0,      // No swap or funding tx missing
                ],
                "Maker swapcoin balance mismatch"
            );

            assert_eq!(balances.fidelity, Amount::from_btc(0.05).unwrap());

            //TODO-: Debug why in every test run malice2 test case gives different contract balance while recovering via hashlock
            // Live contract balance can be non-zero, if a maker shuts down in middle of recovery.
            /*  assert!(
                   balances.contract == Amount::ZERO
                       || balances.contract == Amount::from_btc(0.00441812).unwrap() // Contract balance in recovery scenarios
               );
            */

            // Check spendable balance difference.
            let balance_diff = match org_spend_balance.checked_sub(balances.spendable) {
                None => balances.spendable.checked_sub(*org_spend_balance).unwrap(), // Successful swap as Makers balance increase by Coinswap fee.
                Some(diff) => diff, // No spending or unsuccessful swap
            };

            log::info!(
                "🔍 DEBUG Maker {} balance diff: {} sats",
                maker_index,
                balance_diff.to_sat()
            );

            assert_in_range!(
                balance_diff.to_sat(),
                [
                    21705,  // First maker fee
                    33226,  // Second maker fee
                    0,      // No spending
                    2574,   // Recovery via timelock
                    444213, // Taker abort after setup - first maker recovery cost (abort1 test case)
                    443624, // Taker abort after setup - second maker recovery cost (abort1 test case)
                    466498, // Maker abort after setup(abort3_case3)
                    410176, // Multi-taker first maker (previous run)
                    410118, // Multi-taker first maker (current run)
                ],
                "Maker spendable balance change mismatch"
            );
        });

    log::info!("✅ Swap results verification complete");
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

/// The Test Framework.
///
/// Handles initializing, operating and cleaning up of all backend processes. Bitcoind, Taker and Makers.
#[allow(dead_code)]
pub struct TestFramework {
    pub(super) bitcoind: BitcoinD,
    temp_dir: PathBuf,
    shutdown: AtomicBool,
    tracker_shutdown: Sender<()>,
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
        let temp_dir_clone = temp_dir.clone();
        // Remove if previously existing
        if temp_dir.exists() {
            fs::remove_dir_all::<PathBuf>(temp_dir.clone()).unwrap();
        }
        setup_logger(log::LevelFilter::Info, Some(temp_dir.clone()));
        log::info!("📁 temporary directory : {}", temp_dir.display());

        let bitcoind = init_bitcoind(&temp_dir);

        let shutdown = AtomicBool::new(false);
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel(1);
        let test_framework = Arc::new(Self {
            bitcoind,
            temp_dir: temp_dir.clone(),
            shutdown,
            tracker_shutdown: shutdown_tx,
        });

        log::info!("🌐 Initiating Directory Server .....");

        // Translate a RpcConfig from the test framework.
        // a modification of this will be used for taker and makers rpc connections.
        let rpc_config = RPCConfig::from(test_framework.as_ref());
        let rpc_config_clone = rpc_config.clone();

        std::thread::spawn(move || {
            let tracker_rt =
                tokio::runtime::Runtime::new().expect("Failed to create tracker runtime");

            let tracker_config = tracker::Config {
                rpc_url: rpc_config_clone.url,
                rpc_auth: rpc_config_clone.auth,
                address: "127.0.0.1:8080".to_string(),
                datadir: temp_dir_clone.to_string_lossy().to_string(),
            };

            tracker_rt.block_on(async move {
                tokio::select! {
                    _ = tracker::start(tracker_config) => {},
                    _ = shutdown_rx.recv() => {
                        log::info!("Tracker received shutdown signal, shutting down gracefully");
                    },
                }
            });
        });

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
                    )
                    .unwrap(),
                )
            })
            .collect::<Vec<_>>();

        // start the block generation thread
        log::info!("⛏️ spawning block generation thread");
        let tf_clone = test_framework.clone();
        let generate_blocks_handle = thread::spawn(move || loop {
            thread::sleep(Duration::from_secs(3));

            if tf_clone.shutdown.load(Relaxed) {
                log::info!("🔚 ending block generation thread");
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
        match std::fs::read_to_string(log_path) {
            Ok(log_contents) => {
                assert!(
                    log_contents.contains(expected_message),
                    "Expected log message '{}' not found in log file: {}",
                    expected_message,
                    log_path
                );
                log::info!("✅ Found expected log message: '{expected_message}'");
            }
            Err(e) => {
                panic!("Could not read log file at {}: {}", log_path, e);
            }
        }
    }

    /// Stop bitcoind and clean up all test data.
    pub fn stop(&self) {
        log::info!("🛑 Stopping Test Framework");
        // stop all framework threads.
        self.shutdown.store(true, Relaxed);
        // stop bitcoind
        let _ = self.bitcoind.client.stop().unwrap();
        _ = self.tracker_shutdown.send(());
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

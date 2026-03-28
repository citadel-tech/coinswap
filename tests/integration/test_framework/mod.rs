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

use bip39::rand;
use bitcoin::Amount;
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

use nostr_rs_relay::{config, server::start_server};

use flate2::read::GzDecoder;
use tar::Archive;

use bitcoind::{
    bitcoincore_rpc::{Auth, RpcApi},
    BitcoinD,
};

use coinswap::{
    maker::{MakerBehavior, MakerServer, MakerServerConfig},
    protocol::common_messages::ProtocolVersion,
    taker::{Taker, TakerBehavior, TakerInitConfig},
    utill::setup_logger,
    wallet::{AddressType, RPCConfig},
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
    conf.args.push("-rest=1"); // required for watchtower REST backend
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
                    .unwrap_or_else(|_| "http://170.75.166.88/bitcoin-binaries".to_owned());
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

/// Wait for all makers to complete setup, with a timeout.
///
/// Panics if any maker's `is_setup_complete` flag doesn't become true within `timeout_secs`.
#[allow(dead_code)]
pub fn wait_for_makers_setup(makers: &[Arc<MakerServer>], timeout_secs: u64) {
    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);
    for (i, maker) in makers.iter().enumerate() {
        while !maker.is_setup_complete.load(Relaxed) {
            if start.elapsed() > timeout {
                panic!(
                    "Maker {} did not complete setup within {} seconds",
                    i, timeout_secs
                );
            }
            log::info!("Waiting for maker {} setup completion", i);
            thread::sleep(Duration::from_secs(5));
        }
    }
}

/// Fund taker and verify balance
#[allow(dead_code)]
pub fn fund_taker(
    taker: &Taker,
    bitcoind: &bitcoind::BitcoinD,
    utxo_count: u32,
    utxo_value: Amount,
    address_type: AddressType,
) -> Amount {
    log::info!("💰 Funding Taker...");

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
        "Taker funded successfully. Regular: {}, Spendable: {}",
        balances.regular, balances.spendable
    );

    balances.spendable
}

/// Fund makers and verify their balances
#[allow(dead_code)]
pub fn fund_makers(
    makers: &[Arc<MakerServer>],
    bitcoind: &bitcoind::BitcoinD,
    utxo_count: u32,
    utxo_value: Amount,
    address_type: AddressType,
) -> Vec<Amount> {
    log::info!("💰 Funding Makers...");

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

        assert!(
            balances.regular >= expected_regular,
            "Maker regular balance {} should be >= expected {}",
            balances.regular,
            expected_regular
        );

        info!(
            "Maker funded successfully. Regular: {}, Fidelity: {}",
            balances.regular, balances.fidelity
        );

        spendable_balances.push(balances.spendable);
    }

    spendable_balances
}

/// Verify maker pre-swap balances
#[allow(dead_code)]
pub fn verify_maker_pre_swap_balances(makers: &[Arc<MakerServer>]) -> Vec<Amount> {
    let mut maker_spendable_balance = Vec::new();

    info!("Testing maker balance verification");

    for (i, maker) in makers.iter().enumerate() {
        let wallet = maker.wallet.read().unwrap();
        let balances = wallet.get_balances().unwrap();

        info!(
            "Maker {} balances: Regular: {}, Swap: {}, Contract: {}, Fidelity: {}, Spendable: {}",
            i,
            balances.regular,
            balances.swap,
            balances.contract,
            balances.fidelity,
            balances.spendable
        );

        // Regular balance after fidelity bond creation
        let regular = balances.regular.to_sat();
        assert!(
            regular == 14999516,
            "Maker regular balance check after fidelity bond creation: {}",
            regular
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
            "Maker {} should have spendable balance",
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
    pub(super) nostr_relay_url: String,
    shutdown: AtomicBool,
    nostr_relay_shutdown: mpsc::Sender<()>,
    nostr_relay_handle: Option<JoinHandle<()>>,
}

impl TestFramework {
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

    /// Initialize test framework for protocol testing.
    ///
    /// This creates Taker and MakerServer instances that support
    /// both Legacy (ECDSA) and Taproot (MuSig2) protocols using message types.
    #[allow(clippy::type_complexity)]
    pub fn init(
        makers_config_map: Vec<(u16, Option<u16>)>,
        taker_behavior: Vec<TakerBehavior>,
        maker_behaviors: Vec<MakerBehavior>,
    ) -> (Arc<Self>, Vec<Taker>, Vec<Arc<MakerServer>>, JoinHandle<()>) {
        // Setup directory — use a unique suffix so tests can run in parallel
        let unique_id = format!("coinswap-{}", rand::random::<u64>());
        let temp_dir = env::temp_dir().join(unique_id);
        // Remove if previously existing
        if temp_dir.exists() {
            fs::remove_dir_all::<PathBuf>(temp_dir.clone()).unwrap();
        }
        setup_logger(log::LevelFilter::Info, Some(temp_dir.clone()));
        log::info!("📁 temporary directory : {}", temp_dir.display());

        let port_zmq = 28332 + rand::random::<u16>() % 1000;

        let zmq_addr = format!("tcp://127.0.0.1:{port_zmq}");

        let bitcoind = init_bitcoind(&temp_dir, zmq_addr.clone());

        let nostr_port = 8000 + rand::random::<u16>() % 1000;
        let nostr_relay_url = format!("ws://127.0.0.1:{nostr_port}");
        let (nostr_relay_shutdown, nostr_relay_handle) = spawn_nostr_relay(&temp_dir, nostr_port);
        wait_for_relay_healthy(nostr_port);

        let shutdown = AtomicBool::new(false);
        let test_framework = Arc::new(Self {
            bitcoind,
            temp_dir: temp_dir.clone(),
            nostr_relay_url: nostr_relay_url.clone(),
            shutdown,
            nostr_relay_shutdown,
            nostr_relay_handle: Some(nostr_relay_handle),
        });

        // Translate a RpcConfig from the test framework.
        let rpc_config = RPCConfig::from(test_framework.as_ref());

        // Create the Takers
        let takers = taker_behavior
            .into_iter()
            .enumerate()
            .map(|(i, behavior)| {
                let taker_id = format!("taker{}", i + 1);
                let config = TakerInitConfig::default()
                    .with_data_dir(temp_dir.join(&taker_id))
                    .with_wallet_name(taker_id)
                    .with_rpc_config(rpc_config.clone())
                    .with_zmq_addr(zmq_addr.clone())
                    .with_nostr_relays(vec![nostr_relay_url.clone()]);
                let mut taker = Taker::init(config).unwrap();
                taker.behavior = behavior;
                taker
            })
            .collect::<Vec<_>>();

        let mut base_rpc_port = 4500 + (rand::random::<u16>() % 5000);
        let base_maker_port = 10000 + rand::random::<u16>() % 40000;

        // Create the MakerServers with message handling
        let makers = makers_config_map
            .into_iter()
            .enumerate()
            .map(|(i, (_network_port, _socks_port))| {
                base_rpc_port += 1;
                let network_port = base_maker_port + i as u16;
                let maker_id = format!("maker{}", network_port);
                thread::sleep(Duration::from_secs(5)); // Avoid resource unavailable error

                let config = MakerServerConfig {
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
                    nostr_relays: vec![nostr_relay_url.clone()],
                    ..MakerServerConfig::default()
                };

                let mut server = MakerServer::init(config).unwrap();
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

        log::info!("✅ Test Framework initialization complete");

        (test_framework, takers, makers, generate_blocks_handle)
    }

    /// Stop bitcoind, nostr relay, and clean up all test data.
    pub fn stop(&self) {
        log::info!("🛑 Stopping Test Framework");
        self.shutdown.store(true, Relaxed);
        _ = self.nostr_relay_shutdown.send(());
        let _ = self.bitcoind.client.stop().unwrap();
        std::thread::sleep(std::time::Duration::from_secs(3));
        if self.temp_dir.exists() {
            let _ = fs::remove_dir_all(&self.temp_dir);
        }
    }
}

impl Drop for TestFramework {
    fn drop(&mut self) {
        self.shutdown.store(true, Relaxed);
        _ = self.nostr_relay_shutdown.send(());
        if let Some(handle) = self.nostr_relay_handle.take() {
            if let Err(e) = handle.join() {
                log::error!("Nostr relay thread join failed: {:?}", e);
            }
        }
        let _ = self.bitcoind.client.stop();
        std::thread::sleep(std::time::Duration::from_secs(3));
        if self.temp_dir.exists() {
            let _ = fs::remove_dir_all(&self.temp_dir);
        }
    }
}

/// Handle returned by [`spawn_tracker_logger`] to stop the background thread.
pub struct TrackerLoggerHandle {
    shutdown: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl TrackerLoggerHandle {
    /// Signal the background logger to stop and wait for it to finish.
    #[allow(dead_code)]
    pub fn stop(mut self) {
        self.shutdown.store(true, Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for TrackerLoggerHandle {
    fn drop(&mut self) {
        self.shutdown.store(true, Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Spawn a background thread that periodically reads the swap tracker CBOR file
/// from `data_dir` and logs its contents at INFO level.
///
/// Usage in a test:
/// ```ignore
/// let logger = spawn_tracker_logger(temp_dir.join("taker1"), Duration::from_secs(5));
/// // ... run test ...
/// logger.stop();
/// ```
#[allow(dead_code)]
pub fn spawn_tracker_logger(data_dir: PathBuf, interval: Duration) -> TrackerLoggerHandle {
    use coinswap::taker::swap_tracker::SwapTracker;

    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_clone = shutdown.clone();

    let handle = thread::spawn(move || {
        while !shutdown_clone.load(Relaxed) {
            thread::sleep(interval);
            if shutdown_clone.load(Relaxed) {
                break;
            }
            match SwapTracker::load_or_create(&data_dir) {
                Ok(tracker) => tracker.log_state(),
                Err(e) => log::warn!("[TrackerLogger] Failed to load tracker: {:?}", e),
            }
        }
    });

    TrackerLoggerHandle {
        shutdown,
        handle: Some(handle),
    }
}

fn spawn_nostr_relay(temp_dir: &Path, port: u16) -> (mpsc::Sender<()>, JoinHandle<()>) {
    let data_dir = temp_dir.join("nostr-relay");
    std::fs::create_dir_all(&data_dir).unwrap();

    let mut settings = config::Settings::default();
    settings.network.address = "127.0.0.1".to_string();
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

fn wait_for_relay_healthy(port: u16) {
    let addr = format!("127.0.0.1:{port}");
    let timeout = Duration::from_secs(10);
    let start = Instant::now();

    while start.elapsed() < timeout {
        if TcpStream::connect(&addr).is_ok() {
            log::info!("Nostr relay is alive on port {port}");
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    log::warn!("Nostr relay did not become healthy on port {port} within 10s");
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

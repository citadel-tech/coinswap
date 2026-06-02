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
    wallet::{
        AddressType, BackendConfig, BitcoindBackend, BlockchainBackend, ElectrumBackend,
        ElectrumConfig, RPCConfig,
    },
};
use electrsd::ElectrsD;
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
                               // Bitcoin Core 28 changed `getblockchaininfo`'s `warnings` field to an array of strings;
                               // electrs 0.9.11 (used in the electrum-only test) still expects a string and falls over.
                               // The deprecation flag restores the legacy single-string format.
    conf.args.push("-deprecatedrpc=warnings");
    let raw_tx = format!("-zmqpubrawtx={}", zmq_addr);
    conf.args.push(&raw_tx);
    let block_hash = format!("-zmqpubrawblock={}", zmq_addr);
    conf.args.push(&block_hash);
    // P2P always enabled — needed so electrs can attach via `--daemon-p2p-addr` in
    // electrum-only tests; harmless for tests that don't use electrs.
    conf.p2p = bitcoind::P2P::Yes;
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

/// Spawn an electrs process attached to `bitcoind`. The bitcoind instance must
/// have been started with P2P enabled (see [`init_bitcoind`] which now does so).
///
/// The returned [`ElectrsD`] owns the electrs child process and kills it on drop.
#[allow(dead_code)]
pub(crate) fn init_electrsd(bitcoind: &BitcoinD, datadir: &std::path::Path) -> ElectrsD {
    let exe = electrsd::exe_path().expect(
        "no electrs binary available: set ELECTRS_EXEC or enable the electrs_0_9_11 feature",
    );
    let mut conf = electrsd::Conf::default();
    let electrs_dir = datadir.join("electrs");
    std::fs::create_dir_all(&electrs_dir).ok();
    conf.staticdir = Some(electrs_dir);
    // Surface electrs stderr only when explicitly requested via env var, to keep test output clean.
    conf.view_stderr = std::env::var("ELECTRS_LOG").is_ok();
    let electrsd = ElectrsD::with_conf(exe, bitcoind, &conf).expect("failed to spawn electrs");
    log::info!("🔌 electrs spawned at {}", electrsd.electrum_url);
    electrsd
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
pub fn wait_for_makers_setup<B: BlockchainBackend>(
    makers: &[Arc<MakerServer<B>>],
    timeout_secs: u64,
) {
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
pub fn fund_taker<B: BlockchainBackend>(
    taker: &Taker<B>,
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

    // Poll sync until the wallet observes the expected balance. With a Bitcoin Core backend
    // the first iteration succeeds immediately; with an Electrum backend the indexer needs a
    // moment to pick up the new block.
    let expected_regular = utxo_value * utxo_count.into();
    let balances = wait_for_balance(taker.get_wallet(), expected_regular, 30);
    assert_eq!(balances.regular, expected_regular);

    info!(
        "Taker funded successfully. Regular: {}, Spendable: {}",
        balances.regular, balances.spendable
    );

    balances.spendable
}

/// Poll a wallet, calling `sync_and_save`, until its `regular` balance reaches `expected_regular`
/// or `timeout_secs` elapses. Returns the last observed balances either way.
fn wait_for_balance<B: BlockchainBackend>(
    wallet: &std::sync::Arc<std::sync::RwLock<coinswap::wallet::Wallet<B>>>,
    expected_regular: Amount,
    timeout_secs: u64,
) -> coinswap::wallet::Balances {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let mut last;
    loop {
        {
            let mut w = wallet.write().unwrap();
            w.sync_and_save().unwrap();
            last = w.get_balances().unwrap();
        }
        if last.regular >= expected_regular || Instant::now() >= deadline {
            return last;
        }
        thread::sleep(Duration::from_millis(500));
    }
}

/// Fund makers and verify their balances
#[allow(dead_code)]
pub fn fund_makers<B: BlockchainBackend>(
    makers: &[Arc<MakerServer<B>>],
    bitcoind: &bitcoind::BitcoinD,
    utxo_count: u32,
    utxo_value: Amount,
    address_type: AddressType,
) -> Vec<Amount> {
    log::info!("💰 Funding Makers...");

    let mut spendable_balances = Vec::new();

    for maker in makers {
        // Send funds with the wallet locked just long enough to derive each address.
        for _ in 0..utxo_count {
            let mut wallet = maker.wallet.write().unwrap();
            let addr = wallet.get_next_external_address(address_type).unwrap();
            drop(wallet);
            send_to_address(bitcoind, &addr, utxo_value);
        }

        generate_blocks(bitcoind, 1);
        let expected_regular = utxo_value * utxo_count.into();
        let balances = wait_for_balance(&maker.wallet, expected_regular, 30);

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
pub fn verify_maker_pre_swap_balances<B: BlockchainBackend>(
    makers: &[Arc<MakerServer<B>>],
) -> Vec<Amount> {
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
    /// Present only in tests started via [`TestFramework::init_electrum`]; otherwise `None`.
    /// Kept alive here so the electrs child process lives for the duration of the test.
    pub(super) electrsd: Option<ElectrsD>,
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
            electrsd: None,
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
                    .with_backend(BackendConfig::Bitcoind(RPCConfig {
                        zmq_addr: zmq_addr.clone(),
                        wallet_name: taker_id,
                        ..rpc_config.clone()
                    }))
                    .with_nostr_relays(vec![nostr_relay_url.clone()]);
                let mut taker = Taker::<BitcoindBackend>::init(config).unwrap();
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
                    base_fee: 500,
                    amount_relative_fee_pct: 0.0025,
                    time_relative_fee_pct: 0.0001,
                    min_swap_amount: 10_000,
                    required_confirms: 1,
                    supported_protocols: vec![ProtocolVersion::Legacy, ProtocolVersion::Taproot],
                    fidelity_amount: 5_000_000, // 0.05 BTC
                    fidelity_timelock: 950,     // ~950 blocks for test
                    network: bitcoin::Network::Regtest,
                    backend: BackendConfig::Bitcoind(RPCConfig {
                        zmq_addr: zmq_addr.clone(),
                        wallet_name: maker_id,
                        ..rpc_config.clone()
                    }),
                    nostr_relays: vec![nostr_relay_url.clone()],
                    ..MakerServerConfig::default()
                };

                let mut server = MakerServer::<BitcoindBackend>::init(config).unwrap();
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
            if let Some(elec) = tf_clone.electrsd.as_ref() {
                let _ = elec.trigger();
            }
        });

        log::info!("✅ Test Framework initialization complete");

        (test_framework, takers, makers, generate_blocks_handle)
    }

    /// Electrum-mode variant of [`TestFramework::init`].
    ///
    /// Spawns bitcoind (the source of funds + chain) and an electrs indexer attached to it.
    /// Both the taker and the makers are constructed with `init_electrum(...)` so the
    /// watch-tower and offer-sync code paths exercise the Electrum branch end-to-end and
    /// don't touch Bitcoin Core RPC/REST/ZMQ at all.
    #[allow(clippy::type_complexity)]
    pub fn init_electrum(
        makers_config_map: Vec<(u16, Option<u16>)>,
        taker_behavior: Vec<TakerBehavior>,
        maker_behaviors: Vec<MakerBehavior>,
    ) -> (
        Arc<Self>,
        Vec<Taker<ElectrumBackend>>,
        Vec<Arc<MakerServer<ElectrumBackend>>>,
        JoinHandle<()>,
    ) {
        let unique_id = format!("coinswap-elec-{}", rand::random::<u64>());
        let temp_dir = env::temp_dir().join(unique_id);
        if temp_dir.exists() {
            fs::remove_dir_all::<PathBuf>(temp_dir.clone()).unwrap();
        }
        setup_logger(log::LevelFilter::Info, Some(temp_dir.clone()));
        log::info!("📁 temporary directory : {}", temp_dir.display());

        // bitcoind is launched only as a regtest funding source. ZMQ is unused in electrum
        // mode but the binary still parses the flag, so we hand it a unique placeholder.
        let port_zmq = 28332 + rand::random::<u16>() % 1000;
        let zmq_addr = format!("tcp://127.0.0.1:{port_zmq}");

        let bitcoind = init_bitcoind(&temp_dir, zmq_addr);
        let electrsd = init_electrsd(&bitcoind, &temp_dir);
        // electrum-client expects a tcp:// or ssl:// scheme; ElectrsD reports `host:port` only.
        let electrum_url = format!("tcp://{}", electrsd.electrum_url);

        let nostr_port = 8000 + rand::random::<u16>() % 1000;
        let nostr_relay_url = format!("ws://127.0.0.1:{nostr_port}");
        let (nostr_relay_shutdown, nostr_relay_handle) = spawn_nostr_relay(&temp_dir, nostr_port);
        wait_for_relay_healthy(nostr_port);

        // Give electrs a moment to do its initial index of the 101 blocks bitcoind already mined.
        thread::sleep(Duration::from_secs(2));
        let _ = electrsd.trigger();
        thread::sleep(Duration::from_secs(1));

        let shutdown = AtomicBool::new(false);
        let test_framework = Arc::new(Self {
            bitcoind,
            electrsd: Some(electrsd),
            temp_dir: temp_dir.clone(),
            nostr_relay_url: nostr_relay_url.clone(),
            shutdown,
            nostr_relay_shutdown,
            nostr_relay_handle: Some(nostr_relay_handle),
        });

        let takers = taker_behavior
            .into_iter()
            .enumerate()
            .map(|(i, behavior)| {
                let taker_id = format!("taker{}", i + 1);
                let config = TakerInitConfig::default()
                    .with_data_dir(temp_dir.join(&taker_id))
                    .with_backend(BackendConfig::Electrum(ElectrumConfig {
                        url: electrum_url.clone(),
                        wallet_name: taker_id,
                    }))
                    .with_nostr_relays(vec![nostr_relay_url.clone()]);
                let mut taker = Taker::<ElectrumBackend>::init(config).unwrap();
                taker.behavior = behavior;
                taker
            })
            .collect::<Vec<_>>();

        let mut base_rpc_port = 4500 + (rand::random::<u16>() % 5000);
        let base_maker_port = 10000 + rand::random::<u16>() % 40000;

        let makers = makers_config_map
            .into_iter()
            .enumerate()
            .map(|(i, (_network_port, _socks_port))| {
                base_rpc_port += 1;
                let network_port = base_maker_port + i as u16;
                let maker_id = format!("maker{}", network_port);
                thread::sleep(Duration::from_secs(5));

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
                    fidelity_amount: 5_000_000,
                    fidelity_timelock: 950,
                    network: bitcoin::Network::Regtest,
                    backend: BackendConfig::Electrum(ElectrumConfig {
                        url: electrum_url.clone(),
                        wallet_name: maker_id,
                    }),
                    nostr_relays: vec![nostr_relay_url.clone()],
                    ..MakerServerConfig::default()
                };

                let mut server = MakerServer::<ElectrumBackend>::init(config).unwrap();
                server.behavior = maker_behaviors.get(i).copied().unwrap_or_default();
                Arc::new(server)
            })
            .collect::<Vec<_>>();

        // Block generation thread — also triggers electrs to index new blocks promptly.
        log::info!("⛏️ Spawning block generation thread (electrum mode)");
        let tf_clone = test_framework.clone();
        let generate_blocks_handle = thread::spawn(move || loop {
            thread::sleep(Duration::from_secs(3));
            if tf_clone.shutdown.load(Relaxed) {
                log::info!("🔚 Ending block generation thread");
                return;
            }
            generate_blocks(&tf_clone.bitcoind, 10);
            if let Some(elec) = tf_clone.electrsd.as_ref() {
                let _ = elec.trigger();
            }
        });

        log::info!("✅ Test Framework (electrum) initialization complete");

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

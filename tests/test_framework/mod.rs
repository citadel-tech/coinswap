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

use bitcoin::Amount;
use std::{
    env,
    fs::{self, create_dir_all, File},
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process,
    sync::{
        atomic::{AtomicBool, Ordering::Relaxed},
        mpsc::{self, Receiver, Sender},
        Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use flate2::read::GzDecoder;
use tar::Archive;

use bitcoind::{
    bitcoincore_rpc::{Auth, RpcApi},
    BitcoinD,
};

use coinswap::{
    maker::{Maker, MakerBehavior},
    market::directory::{start_directory_server, DirectoryServer},
    taker::{Taker, TakerBehavior},
    utill::{setup_logger, ConnectionType},
    wallet::RPCConfig,
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
    log::info!("bitcoind datadir: {:?}", conf.staticdir.as_ref().unwrap());
    log::info!("bitcoind configuration: {:?}", conf.args);

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

    log::info!("Executable path: {exe_path:?}");

    let bitcoind = BitcoinD::with_conf(exe_path, &conf).unwrap();

    // Generate initial 101 blocks
    generate_blocks(&bitcoind, 101);
    log::info!("bitcoind initiated!!");

    bitcoind
}

/// Generate Blocks in regtest node.
// TODO: Rethink this approach considering the resource unavailability of RPC and wallet rescanning.
//       Block generation in regtest halts everything, causing a major disruption in rescanning.
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

// Waits until the mpsc::Receiver<String> receives the expected message.
pub(crate) fn await_message(rx: &Receiver<String>, expected_message: &str) {
    loop {
        let log_message = rx.recv().expect("Failure from Sender side");
        if log_message.contains(expected_message) {
            break;
        }
    }
}

// Start the DNS server based on given connection type and considers data directory for the server.
#[allow(dead_code)]
pub(crate) fn start_dns(data_dir: &std::path::Path, bitcoind: &BitcoinD) -> process::Child {
    let (stdout_sender, stdout_recv): (Sender<String>, Receiver<String>) = mpsc::channel();

    let (stderr_sender, stderr_recv): (Sender<String>, Receiver<String>) = mpsc::channel();

    let mut args = vec!["--data-directory", data_dir.to_str().unwrap()];

    // RPC authentication (user:password) from the cookie file
    let cookie_file_path = Path::new(&bitcoind.params.cookie_file);
    let rpc_auth = fs::read_to_string(cookie_file_path).expect("failed to read from file");
    args.push("--USER:PASSWORD");
    args.push(&rpc_auth);

    // Full node address for RPC connection
    let rpc_address = bitcoind.params.rpc_socket.to_string();
    args.push("--ADDRESS:PORT");
    args.push(&rpc_address);

    let mut directoryd_process = process::Command::new(env!("CARGO_BIN_EXE_directoryd"))
        .args(args) // THINK: Passing network to avoid mitosis problem..
        .stdout(process::Stdio::piped())
        .stderr(process::Stdio::piped())
        .spawn()
        .expect("Failed to spawn directoryd process");
    let stderr = directoryd_process.stderr.take().unwrap();
    let stdout = directoryd_process.stdout.take().unwrap();

    // stderr thread
    thread::spawn(move || {
        let reader = BufReader::new(stderr);
        if let Some(line) = reader.lines().map_while(Result::ok).next() {
            println!("{line}");
            let _ = stderr_sender.send(line);
        }
    });

    // stdout thread
    thread::spawn(move || {
        let reader = BufReader::new(stdout);

        for line in reader.lines().map_while(Result::ok) {
            println!("{line}");
            if stdout_sender.send(line).is_err() {
                break;
            }
        }
    });

    // wait for some time to check for any stderr
    if let Ok(stderr) = stderr_recv.recv_timeout(std::time::Duration::from_secs(10)) {
        panic!("Error: {:?}", stderr)
    }

    await_message(&stdout_recv, "RPC socket binding successful");
    log::info!("DNS Server Started");

    directoryd_process
}

#[allow(dead_code)]
pub fn fund_and_verify_taker(
    taker: &mut Taker,
    bitcoind: &BitcoinD,
    utxo_count: u32,
    utxo_value: Amount,
) -> Amount {
    log::info!("Funding Takers...");

    // Fund the Taker with 3 utxos of 0.05 btc each.
    for _ in 0..utxo_count {
        let taker_address = taker.get_wallet_mut().get_next_external_address().unwrap();
        send_to_address(bitcoind, &taker_address, utxo_value);
    }

    // confirm balances
    generate_blocks(bitcoind, 1);

    //------Basic Checks-----

    let wallet = taker.get_wallet_mut();
    // Assert external address index reached to 3.
    assert_eq!(wallet.get_external_index(), &utxo_count);

    let _ = wallet.sync();

    // Check if utxo list looks good.
    // TODO: Assert other interesting things from the utxo list.

    let balances = wallet.get_balances().unwrap();

    // TODO: Think about this: utxo_count*utxo_amt.
    assert_eq!(balances.regular, Amount::from_btc(0.15).unwrap());
    assert_eq!(balances.fidelity, Amount::ZERO);
    assert_eq!(balances.swap, Amount::ZERO);
    assert_eq!(balances.contract, Amount::ZERO);

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

    log::info!("Funding Makers...");

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
        wallet.sync().unwrap();

        let balances = wallet.get_balances().unwrap();

        // TODO: Think about this: utxo_count*utxo_amt.
        assert_eq!(balances.regular, Amount::from_btc(0.20).unwrap());
        assert_eq!(balances.fidelity, Amount::ZERO);
        assert_eq!(balances.swap, Amount::ZERO);
        assert_eq!(balances.contract, Amount::ZERO);
    });
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

        assert!(
            balances.regular == Amount::from_btc(0.14497).unwrap() // Successful coinswap
                || balances.regular == Amount::from_btc(0.14993232).unwrap() // Recovery via timelock
                || balances.regular == Amount::from_btc(0.15).unwrap(), // No spending
            "Taker seed balance mismatch"
        );

        assert!(
            balances.swap == Amount::from_btc(0.00438642).unwrap() // Successful coinswap
                || balances.swap == Amount::ZERO, // Unsuccessful coinswap
            "Taker swapcoin balance mismatch"
        );

        assert_eq!(balances.contract, Amount::ZERO);
        assert_eq!(balances.fidelity, Amount::ZERO);

        // Check balance difference
        let balance_diff = org_taker_spend_balance
            .checked_sub(balances.spendable)
            .unwrap();

        assert!(
            balance_diff == Amount::from_sat(64358) // Successful coinswap
                || balance_diff == Amount::from_sat(6768) // Recovery via timelock
                || balance_diff == Amount::from_sat(503000) // Spent swapcoin
                || balance_diff == Amount::ZERO, // No spending
            "Taker spendable balance change mismatch"
        );
    }

    // Check Maker balances
    makers
        .iter()
        .zip(org_maker_spend_balances.iter())
        .for_each(|(maker, org_spend_balance)| {
            let wallet = maker.get_wallet().read().unwrap();
            let balances = wallet.get_balances().unwrap();

            assert!(
                balances.regular == Amount::from_btc(0.14557358).unwrap() // First maker on successful coinswap
                    || balances.regular == Amount::from_btc(0.14532500).unwrap() // Second maker on successful coinswap
                    || balances.regular == Amount::from_btc(0.14999).unwrap() // No spending
                    || balances.regular == Amount::from_btc(0.14992232).unwrap() // Recovery via timelock
                    || balances.regular == Amount::from_btc(0.14090858).unwrap(), // Mutli-taker case
                "Maker seed balance mismatch"
            );

            assert!(
                balances.swap == Amount::from_btc(0.005).unwrap() // First maker
                    || balances.swap == Amount::from_btc(0.00463500).unwrap() // Second maker
                    || balances.swap == Amount::ZERO, // No swap or funding tx missing
                "Maker swapcoin balance mismatch"
            );

            assert_eq!(balances.fidelity, Amount::from_btc(0.05).unwrap());

            // Live contract balance can be non-zero, if a maker shuts down in middle of recovery.
            assert!(
                balances.contract == Amount::ZERO
                    || balances.contract == Amount::from_btc(0.00460500).unwrap() // For the first maker in hop
                    || balances.contract == Amount::from_btc(0.00435642).unwrap() // For the second maker in hop
            );

            // Check spendable balance difference.
            let balance_diff = match org_spend_balance.checked_sub(balances.spendable) {
                None => balances.spendable.checked_sub(*org_spend_balance).unwrap(), // Successful swap as Makers balance increase by Coinswap fee.
                Some(diff) => diff, // No spending or unsuccessful swap
            };

            assert!(
                balance_diff == Amount::from_sat(33500) // First maker fee
                    || balance_diff == Amount::from_sat(21858) // Second maker fee
                    || balance_diff == Amount::ZERO // No spending
                    || balance_diff == Amount::from_sat(6768) // Recovery via timelock
                    || balance_diff == Amount::from_sat(466500) // TODO: Investigate this value
                    || balance_diff == Amount::from_sat(441642) // TODO: Investigate this value
                    || balance_diff == Amount::from_sat(408142) // Multi-taker first maker
                    || balance_diff == Amount::from_sat(444642), // Multi-taker second maker
                "Maker spendable balance change mismatch"
            );
        });
}

/// The Test Framework.
///
/// Handles initializing, operating and cleaning up of all backend processes. Bitcoind, Taker and Makers.
#[allow(dead_code)]
pub struct TestFramework {
    pub(super) bitcoind: BitcoinD,
    temp_dir: PathBuf,
    shutdown: AtomicBool,
}

impl TestFramework {
    /// Initialize a test-framework environment from given configuration data.
    /// This object holds the reference to backend bitcoind process and RPC.
    /// It takes:
    /// - bitcoind conf.
    /// - a map of [port, [MakerBehavior]]
    /// - optional taker behavior.
    /// - connection type
    ///
    /// Returns ([TestFramework], [Taker], [`Vec<Maker>`]).
    /// Maker's config will follow the pattern given the input HashMap.
    /// If no bitcoind conf is provided, a default value will be used.
    #[allow(clippy::type_complexity)]
    pub fn init(
        makers_config_map: Vec<((u16, Option<u16>), MakerBehavior)>,
        taker_behavior: Vec<TakerBehavior>,
        connection_type: ConnectionType,
    ) -> (
        Arc<Self>,
        Vec<Taker>,
        Vec<Arc<Maker>>,
        Arc<DirectoryServer>,
        JoinHandle<()>,
    ) {
        // Setup directory
        let temp_dir = env::temp_dir().join("coinswap");
        setup_logger(log::LevelFilter::Info, Some(temp_dir.clone()));
        // Remove if previously existing
        if temp_dir.exists() {
            fs::remove_dir_all::<PathBuf>(temp_dir.clone()).unwrap();
        }
        log::info!("temporary directory : {}", temp_dir.display());

        let bitcoind = init_bitcoind(&temp_dir);

        let shutdown = AtomicBool::new(false);
        let test_framework = Arc::new(Self {
            bitcoind,
            temp_dir: temp_dir.clone(),
            shutdown,
        });

        log::info!("Initiating Directory Server .....");

        // Translate a RpcConfig from the test framework.
        // a modification of this will be used for taker and makers rpc connections.
        let rpc_config = RPCConfig::from(test_framework.as_ref());

        let directory_rpc_config = rpc_config.clone();

        let directory_server_instance = Arc::new(
            DirectoryServer::new(Some(temp_dir.join("dns")), Some(connection_type)).unwrap(),
        );
        let directory_server_instance_clone = directory_server_instance.clone();
        thread::spawn(move || {
            start_directory_server(directory_server_instance_clone, Some(directory_rpc_config))
                .unwrap();
        });

        // Create the Taker.
        let taker_rpc_config = rpc_config.clone();
        let takers = taker_behavior
            .into_iter()
            .enumerate()
            .map(|(i, behavior)| {
                Taker::init(
                    Some(temp_dir.join(format!("taker{}", i + 1))),
                    None,
                    Some(taker_rpc_config.clone()),
                    behavior,
                    None,
                    None,
                    Some(connection_type),
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
                        Some(connection_type),
                        behavior,
                    )
                    .unwrap(),
                )
            })
            .collect::<Vec<_>>();

        // start the block generation thread
        log::info!("spawning block generation thread");
        let tf_clone = test_framework.clone();
        let generate_blocks_handle = thread::spawn(move || loop {
            thread::sleep(Duration::from_secs(3));

            if tf_clone.shutdown.load(Relaxed) {
                log::info!("ending block generation thread");
                return;
            }
            // tf_clone.generate_blocks(10);
            generate_blocks(&tf_clone.bitcoind, 10);
        });

        (
            test_framework,
            takers,
            makers,
            directory_server_instance,
            generate_blocks_handle,
        )
    }

    /// Stop bitcoind and clean up all test data.
    pub fn stop(&self) {
        log::info!("Stopping Test Framework");
        // stop all framework threads.
        self.shutdown.store(true, Relaxed);
        // stop bitcoind
        let _ = self.bitcoind.client.stop().unwrap();
    }
}

/// Initializes a [TestFramework] given a [RPCConfig].
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

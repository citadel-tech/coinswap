//! A Framework to write functional tests for the Coinswap Protocol.
//!
//! This framework uses [bitcoind] to automatically spawn regtest node in the background.
//!
//! Spawns one Taker and multiple Makers, with/without special behavior, connect them to bitcoind regtest node,
//! and initializes the database.
//!
//! The tests data are stored in the `tests/temp-files` directory, which is auto-removed after each successful test.
//! Do not invoke [TestFramework::stop] function at the end of the test, to persis this data for debugging.
//!
//! The test data also includes the backend bitcoind data-directory, which is useful for observing the blockchain states after a swap.
//!
//! Checkout `tests/standard_swap.rs` for example of simple coinswap simulation test between 1 Taker and 2 Makers.
use std::{
    collections::HashMap,
    env::{self, consts},
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering::Relaxed},
        Arc, RwLock,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use bitcoind::{bitcoincore_rpc::RpcApi, BitcoinD};
use coinswap::utill::ConnectionType;
use std::{
    io::{BufRead, BufReader},
    process,
    sync::mpsc::{self, Receiver, Sender},
};

use bitcoind::bitcoincore_rpc::Auth;

use coinswap::{
    maker::{Maker, MakerBehavior},
    market::directory::{start_directory_server, DirectoryServer},
    taker::{Taker, TakerBehavior},
    utill::setup_logger,
    wallet::RPCConfig,
};

/// Initiate the bitcoind backend.
pub fn init_bitcoind(datadir: &std::path::Path) -> BitcoinD {
    let mut conf = bitcoind::Conf::default();
    conf.args.push("-txindex=1"); //txindex is must, or else wallet sync won't work.
    conf.staticdir = Some(datadir.join(".bitcoin"));
    log::info!("bitcoind datadir: {:?}", conf.staticdir.as_ref().unwrap());
    log::info!("bitcoind configuration: {:?}", conf.args);

    let os = consts::OS;
    let arch = consts::ARCH;

    let key = "BITCOIND_EXE";
    let curr_dir_path = env::current_dir().unwrap();

    let bitcoind_path = match (os, arch) {
        ("macos", "aarch64") => curr_dir_path.join("bin").join("bitcoind_macos"),
        _ => curr_dir_path.join("bin").join("bitcoind"),
    };
    env::set_var(key, bitcoind_path);
    let exe_path = bitcoind::exe_path().unwrap();

    log::info!("Executable path: {:?}", exe_path);

    let bitcoind = BitcoinD::with_conf(exe_path, &conf).unwrap();

    // Generate initial 101 blocks
    generate_blocks(&bitcoind, 101);
    log::info!("bitcoind initiated!!");

    bitcoind
}

/// Generate Blocks in regtest node.
pub fn generate_blocks(bitcoind: &BitcoinD, n: u64) {
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
pub fn send_to_address(
    bitcoind: &BitcoinD,
    addrs: &bitcoin::Address,
    amount: bitcoin::Amount,
) -> bitcoin::Txid {
    bitcoind
        .client
        .send_to_address(addrs, amount, None, None, None, None, None, None)
        .unwrap()
}

// Waits until the mpsc::Receiver<String> recieves the expected message.
pub fn await_message(rx: &Receiver<String>, expected_message: &str) {
    loop {
        let log_message = rx.recv().expect("Failure from Sender side");
        if log_message.contains(expected_message) {
            break;
        }
    }
}

// Start the DNS server based on given connection type and considers data directory for the server.
#[allow(dead_code)]
pub fn start_dns(
    data_dir: &std::path::Path,
    conn_type: ConnectionType,
    bitcoind: &BitcoinD,
) -> process::Child {
    let (stdout_sender, stdout_recv): (Sender<String>, Receiver<String>) = mpsc::channel();

    let (stderr_sender, stderr_recv): (Sender<String>, Receiver<String>) = mpsc::channel();
    let conn_type = format!("{}", conn_type);

    let mut args = vec![
        "--data-directory",
        data_dir.to_str().unwrap(),
        "--rpc_network",
        "regtest",
    ];

    // RPC authentication (user:password) from the cookie file
    let cookie_file_path = Path::new(&bitcoind.params.cookie_file);
    let rpc_auth = fs::read_to_string(cookie_file_path).expect("failed to read from file");
    args.push("--USER:PASSWORD");
    args.push(&rpc_auth);

    // Full node address for RPC connection
    let rpc_address = bitcoind.params.rpc_socket.to_string();
    args.push("--ADDRESS:PORT");
    args.push(&rpc_address);

    let mut directoryd_process = process::Command::new("./target/debug/directoryd")
        .args(args) // THINK: Passing network to avoid mitosis problem..
        .stdout(process::Stdio::piped())
        .stderr(process::Stdio::piped())
        .spawn()
        .unwrap();

    let stderr = directoryd_process.stderr.take().unwrap();
    let stdout = directoryd_process.stdout.take().unwrap();

    // stderr thread
    thread::spawn(move || {
        let reader = BufReader::new(stderr);
        if let Some(line) = reader.lines().map_while(Result::ok).next() {
            let _ = stderr_sender.send(line);
        }
    });

    // stdout thread
    thread::spawn(move || {
        let reader = BufReader::new(stdout);

        for line in reader.lines().map_while(Result::ok) {
            log::info!("{line}");
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
    /// If no bitcoind conf is provide a default value will be used.
    #[allow(clippy::type_complexity)]
    pub fn init(
        makers_config_map: HashMap<(u16, Option<u16>), MakerBehavior>,
        taker_behavior: TakerBehavior,
        connection_type: ConnectionType,
    ) -> (
        Arc<Self>,
        Arc<RwLock<Taker>>,
        Vec<Arc<Maker>>,
        Arc<DirectoryServer>,
        JoinHandle<()>,
    ) {
        if cfg!(feature = "tor") && connection_type == ConnectionType::TOR {
            coinswap::tor::setup_mitosis();
        }
        setup_logger(log::LevelFilter::Info);
        // Setup directory
        let temp_dir = env::temp_dir().join("coinswap");
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
        let taker = Arc::new(RwLock::new(
            Taker::init(
                Some(temp_dir.join("taker")),
                None,
                Some(taker_rpc_config),
                taker_behavior,
                Some(connection_type),
            )
            .unwrap(),
        ));
        let mut base_rpc_port = 3500; // Random port for RPC connection in tests. (Not used)
                                      // Create the Makers as per given configuration map.
        let makers = makers_config_map
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
            taker,
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
        let network = value.bitcoind.client.get_blockchain_info().unwrap().chain;
        Self {
            url,
            auth,
            network,
            ..Default::default()
        }
    }
}

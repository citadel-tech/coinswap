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
use bitcoin::secp256k1::rand::{distributions::Alphanumeric, thread_rng, Rng};
use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::{Arc, RwLock},
    thread,
    time::Duration,
};

use bitcoin::{Address, Amount};

use bitcoind::{
    bitcoincore_rpc::{Auth, Client, RpcApi},
    BitcoinD, Conf,
};
use coinswap::{
    maker::{Maker, MakerBehavior},
    market::directory::{start_directory_server, DirectoryServer},
    taker::{Taker, TakerBehavior},
    utill::{setup_logger, ConnectionType},
    wallet::RPCConfig,
};

fn get_random_tmp_dir() -> PathBuf {
    let s: String = thread_rng()
        .sample_iter(&Alphanumeric)
        .take(8)
        .map(char::from)
        .collect();
    let path = "/tmp/.coinswap/".to_string() + &s;
    PathBuf::from(path)
}

/// The Test Framework.
///
/// Handles initializing, operating and cleaning up of all backend processes. Bitcoind, Taker and Makers.
#[allow(dead_code)]
pub struct TestFramework {
    bitcoind: BitcoinD,
    temp_dir: PathBuf,
    shutdown: Arc<RwLock<bool>>,
}

impl TestFramework {
    /// Initialize a test-framework environment from given configuration data.
    /// This object holds the reference to backend bitcoind process and RPC.
    /// - bitcoind conf.
    /// - a map of [port, [MakerBehavior]]
    /// - optional taker behavior.
    ///
    /// Returns ([TestFramework], [Taker], [`Vec<Maker>`]).
    /// Maker's config will follow the pattern given the input HashMap.
    /// If no bitcoind conf is provide a default value will be used.
    pub async fn init(
        bitcoind_conf: Option<Conf<'_>>,
        makers_config_map: HashMap<(u16, Option<u16>), MakerBehavior>,
        taker_behavior: Option<TakerBehavior>,
        connection_type: ConnectionType,
    ) -> (
        Arc<Self>,
        Arc<RwLock<Taker>>,
        Vec<Arc<Maker>>,
        Arc<DirectoryServer>,
    ) {
        if cfg!(feature = "tor") && connection_type == ConnectionType::TOR {
            coinswap::tor::setup_mitosis();
        }
        setup_logger();
        // Setup directory
        let temp_dir = get_random_tmp_dir();
        // Remove if previously existing
        if temp_dir.exists() {
            fs::remove_dir_all::<PathBuf>(temp_dir.clone()).unwrap();
        }
        log::info!("temporary directory : {}", temp_dir.display());

        // Initiate the bitcoind backend.
        let mut conf = bitcoind_conf.unwrap_or_default();
        conf.args.push("-txindex=1"); //txindex is must, or else wallet sync won't work.
        conf.staticdir = Some(temp_dir.join(".bitcoin"));
        log::info!("bitcoind configuration: {:?}", conf.args);

        let key = "BITCOIND_EXE";
        let curr_dir_path = std::env::current_dir().unwrap();
        let bitcoind_path = curr_dir_path.join("bin").join("bitcoind");
        std::env::set_var(key, bitcoind_path);
        let exe_path = bitcoind::exe_path().unwrap();

        log::info!("Executable path: {:?}", exe_path);

        let bitcoind = BitcoinD::with_conf(exe_path, &conf).unwrap();

        // Generate initial 101 blocks
        let mining_address = bitcoind
            .client
            .get_new_address(None, None)
            .unwrap()
            .require_network(bitcoind::bitcoincore_rpc::bitcoin::Network::Regtest)
            .unwrap();
        bitcoind
            .client
            .generate_to_address(101, &mining_address)
            .unwrap();
        log::info!("bitcoind initiated!!");
        let shutdown = Arc::new(RwLock::new(false));
        let test_framework = Arc::new(Self {
            bitcoind,
            temp_dir: temp_dir.clone(),
            shutdown,
        });

        log::info!("Initiating Directory Server .....");

        let directory_server_instance =
            Arc::new(DirectoryServer::new(None, Some(connection_type)).unwrap());
        let directory_server_instance_clone = directory_server_instance.clone();
        thread::spawn(move || {
            start_directory_server(directory_server_instance_clone);
        });

        // Translate a RpcConfig from the test framework.
        // a modification of this will be used for taker and makers rpc connections.
        let rpc_config = RPCConfig::from(test_framework.as_ref());

        // Create the Taker.
        let taker_rpc_config = rpc_config.clone();
        let taker = Arc::new(RwLock::new(
            Taker::init(
                Some(temp_dir.clone().join("taker")),
                None,
                Some(taker_rpc_config),
                taker_behavior.unwrap_or_default(),
                Some(connection_type),
            )
            .unwrap(),
        ));
        let mut base_rpc_port = 3500; // Random port for RPC connection in tests. (Not used)
                                      // Create the Makers as per given configuration map.
        let makers = makers_config_map
            .iter()
            .map(|(port, behavior)| {
                base_rpc_port += 1;
                let maker_id = "maker".to_string() + &port.0.to_string(); // ex: "maker6102"
                let maker_rpc_config = rpc_config.clone();
                thread::sleep(Duration::from_secs(5)); // Sleep for some time avoid resource unavailable error.
                Arc::new(
                    Maker::init(
                        Some(temp_dir.clone()),
                        Some(maker_id),
                        Some(maker_rpc_config),
                        Some(port.0),
                        Some(base_rpc_port),
                        port.1,
                        Some(connection_type),
                        *behavior,
                    )
                    .unwrap(),
                )
            })
            .collect::<Vec<_>>();

        // start the block generation thread
        log::info!("spawning block generation thread");
        let tf_clone = test_framework.clone();
        thread::spawn(move || loop {
            thread::sleep(Duration::from_secs(3));
            tf_clone.generate_blocks(10);
            if *tf_clone.shutdown.read().unwrap() {
                log::info!("ending block generation thread");
                return;
            }
        });

        (test_framework, taker, makers, directory_server_instance)
    }

    /// Get the internal bitcoind client reference.
    pub fn get_client(&self) -> &Client {
        &self.bitcoind.client
    }

    /// Generate Blocks in regtest node.
    pub fn generate_blocks(&self, n: u64) {
        let mining_address = self
            .bitcoind
            .client
            .get_new_address(None, None)
            .unwrap()
            .require_network(bitcoind::bitcoincore_rpc::bitcoin::Network::Regtest)
            .unwrap();
        self.bitcoind
            .client
            .generate_to_address(n, &mining_address)
            .unwrap();
    }

    /// Send coins to a bitcoin address.
    pub fn send_to_address(&self, addrs: &Address, amount: Amount) {
        self.bitcoind
            .client
            .send_to_address(addrs, amount, None, None, None, None, None, None)
            .unwrap();
    }

    /// Stop bitcoind and clean up all test data.
    pub fn stop(&self) {
        log::info!("Stopping Test Framework");
        // stop all framework threads.
        *self.shutdown.write().unwrap() = true;
        // stop bitcoind
        let _ = self.bitcoind.client.stop().unwrap();
    }

    pub fn get_block_count(&self) -> u64 {
        self.bitcoind.client.get_block_count().unwrap()
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

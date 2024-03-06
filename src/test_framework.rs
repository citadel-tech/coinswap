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

use crate::{
    maker::{Maker, MakerBehavior},
    taker::{Taker, TakerBehavior},
    utill::{setup_logger, setup_mitosis, str_to_bitcoin_network},
    wallet::RPCConfig,
};
use bitcoind::{
    bitcoincore_rpc::{Auth, RpcApi},
    BitcoinD, Conf,
};

fn get_random_tmp_dir() -> PathBuf {
    let s: String = thread_rng()
        .sample_iter(&Alphanumeric)
        .take(8)
        .map(char::from)
        .collect();
    let path = "/tmp/teleport/tests/temp-files/".to_string() + &s;
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
        makers_config_map: HashMap<(u16, u16), MakerBehavior>,
        taker_behavior: Option<TakerBehavior>,
    ) -> (Arc<Self>, Arc<RwLock<Taker>>, Vec<Arc<Maker>>) {
        setup_mitosis();
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

        // Translate a RpcConfig from the test framework.
        // a modification of this will be used for taker and makers rpc connections.
        let rpc_config = RPCConfig::from(test_framework.as_ref());

        // Create the Taker.
        let taker_rpc_config = rpc_config.clone();
        let taker = Arc::new(RwLock::new(
            Taker::init(
                Some(&temp_dir),
                None,
                Some(taker_rpc_config),
                taker_behavior.unwrap_or_default(),
                makers_config_map.len() as u16,
            )
            .unwrap(),
        ));

        // Create the Makers as per given configuration map.
        let makers = makers_config_map
            .iter()
            .map(|(port, behavior)| {
                let maker_id = "maker".to_string() + &port.0.to_string(); // ex: "maker6102"
                let maker_rpc_config = rpc_config.clone();
                thread::sleep(Duration::from_secs(5)); // Sleep for some time avoid resource unavailable error.
                let tor_port = port.0;
                let socks_port = port.1;
                Arc::new(
                    Maker::init(
                        Some(&temp_dir),
                        Some(maker_id),
                        Some(maker_rpc_config),
                        Some(tor_port),
                        Some(socks_port),
                        *behavior,
                    )
                    .unwrap(),
                )
            })
            .collect::<Vec<_>>();

        // start the block generation thread
        log::info!("spawning block generation thread");
        let tf_clone = test_framework.clone();
        thread::spawn(move || {
            while !*tf_clone.shutdown.read().unwrap() {
                thread::sleep(Duration::from_millis(500));
                tf_clone.generate_1_block();
                log::debug!("created 1 block");
            }
            log::info!("ending block generation thread");
        });

        (test_framework, taker, makers)
    }

    /// Generate 1 block in the backend bitcoind.
    pub fn generate_1_block(&self) {
        let mining_address = self
            .bitcoind
            .client
            .get_new_address(None, None)
            .unwrap()
            .require_network(bitcoind::bitcoincore_rpc::bitcoin::Network::Regtest)
            .unwrap();
        self.bitcoind
            .client
            .generate_to_address(1, &mining_address)
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
        let network = str_to_bitcoin_network(
            value
                .bitcoind
                .client
                .get_blockchain_info()
                .unwrap()
                .chain
                .as_str(),
        );
        Self {
            url,
            auth,
            network,
            ..Default::default()
        }
    }
}

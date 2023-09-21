pub static TEMP_FILES_DIR: &str = "tests/temp-files";
pub static TAKER: &str = "tests/temp-files/taker-wallet";
pub static MAKER1: &str = "tests/temp-files/maker-wallet-1";
pub static MAKER2: &str = "tests/temp-files/maker-wallet-2";
pub static MAKER3: &str = "tests/temp-files/maker-wallet-3";

// Helper function to create new wallet
pub fn create_wallet_and_import(filename: &PathBuf, rpc_config: &RPCConfig) -> Wallet {
    if filename.exists() {
        fs::remove_file(&filename).unwrap();
    }
    let mnemonic = Mnemonic::generate(12).unwrap();
    let seedphrase = mnemonic.to_string();

    let mut wallet = Wallet::init(
        &filename,
        rpc_config,
        seedphrase,
        "".to_string(),
        Some(WalletMode::Testing),
    )
    .unwrap();

    wallet.sync().unwrap();

    wallet
}

use bitcoin::secp256k1::rand::{distributions::Alphanumeric, thread_rng, Rng}; // 0.8

pub fn get_random_tmp_dir() -> PathBuf {
    let s: String = thread_rng()
        .sample_iter(&Alphanumeric)
        .take(8)
        .map(char::from)
        .collect();
    let path = "tests/temp-files/".to_string() + &s;
    PathBuf::from(path)
}

use std::{fs, path::PathBuf};

use bip39::Mnemonic;
use bitcoin::{Address, Amount};

use crate::{
    utill::str_to_bitcoin_network,
    wallet::{RPCConfig, Wallet, WalletMode},
};
use bitcoind::{
    bitcoincore_rpc::{Auth, RpcApi},
    BitcoinD, Conf,
};

pub struct TestFrameWork {
    bitcoind: BitcoinD,
}

impl TestFrameWork {
    pub fn new(conf: Option<Conf>) -> Self {
        let mut conf = conf.unwrap_or_default();
        conf.args.push("-txindex=1");
        let bitcoind = BitcoinD::from_downloaded_with_conf(&conf).unwrap();
        // Generate initial fund
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
        Self { bitcoind }
    }

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

    pub fn send_to_address(&self, addrs: &Address, amount: Amount) {
        self.bitcoind
            .client
            .send_to_address(addrs, amount, None, None, None, None, None, None)
            .unwrap();
    }

    pub fn stop(&self) {
        let _ = self.bitcoind.client.stop().unwrap();
    }

    pub fn get_block_count(&self) -> u64 {
        self.bitcoind.client.get_block_count().unwrap()
    }
}

impl From<&TestFrameWork> for RPCConfig {
    fn from(value: &TestFrameWork) -> Self {
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

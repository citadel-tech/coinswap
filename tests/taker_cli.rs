use bitcoin::{address::NetworkChecked, Address, Amount, Transaction};
use bitcoind::{bitcoincore_rpc::RpcApi, tempfile::env::temp_dir, BitcoinD, Conf};

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    str::FromStr,
};

/// The taker-cli command struct
struct TakerCli {
    data_dir: PathBuf,
    bitcoind: BitcoinD,
}

impl TakerCli {
    /// Construct a new [`TakerCli`] struct that also include initiating bitcoind.
    fn new() -> TakerCli {
        // Initiate the bitcoind backend.

        let temp_dir = temp_dir().join(".coinswap");

        // Remove if previously existing
        if temp_dir.exists() {
            fs::remove_dir_all::<PathBuf>(temp_dir.clone()).unwrap();
        }

        let mut conf = Conf::default();

        conf.args.push("-txindex=1"); //txindex is must, or else wallet sync won't work.
        conf.staticdir = Some(temp_dir.join(".bitcoin"));

        log::info!("bitcoind configuration: {:?}", conf.args);

        let os = std::env::consts::OS;
        let arch = std::env::consts::ARCH;
        let key = "BITCOIND_EXE";
        let curr_dir_path = std::env::current_dir().unwrap();

        let bitcoind_path = match (os, arch) {
            ("macos", "aarch64") => curr_dir_path.join("bin").join("bitcoind_macos"),
            _ => curr_dir_path.join("bin").join("bitcoind"),
        };
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

        let data_dir = temp_dir.join("taker");

        TakerCli { data_dir, bitcoind }
    }

    // Execute a cli-command
    fn execute(&self, cmd: &[&str]) -> String {
        let mut args = vec![
            "--data-directory",
            self.data_dir.as_os_str().to_str().unwrap(),
            "--bitcoin-network",
            "regtest",
            "--connection-type",
            "clearnet",
        ];

        // RPC authentication (user:password) from the cookie file
        let cookie_file_path = Path::new(&self.bitcoind.params.cookie_file);
        let rpc_auth = fs::read_to_string(cookie_file_path).expect("failed to read from file");
        args.push("--USER:PASSWORD");
        args.push(&rpc_auth);

        // Full node address for RPC connection
        let rpc_address = self.bitcoind.params.rpc_socket.to_string();
        args.push("--ADDRESS:PORT");
        args.push(&rpc_address);

        args.push("--WALLET");
        args.push("test_wallet");

        // makers count
        args.push("3");

        // tx_count
        args.push("3");

        // fee_rate
        args.push("1000");

        for arg in cmd {
            args.push(arg);
        }

        let output = Command::new("./target/debug/taker")
            .args(args)
            .output()
            .unwrap();

        // Capture the standard output and error from the command execution
        let mut value = output.stdout;
        let error = output.stderr;

        // Panic if there is any error output
        if !error.is_empty() {
            panic!("Error: {:?}", String::from_utf8(error).unwrap());
        }

        // Remove the `\n` at the end of the output
        value.pop();

        // Convert the output bytes to a UTF-8 string
        let output_string = std::str::from_utf8(&value).unwrap().to_string();

        output_string
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
}

#[test]
fn test_taker_cli() {
    let taker_cli = TakerCli::new();

    // Fund the taker with 3 utxos of 1 BTC each.
    for _ in 0..3 {
        let taker_address = taker_cli.execute(&["get-new-address"]);

        let taker_address: Address<NetworkChecked> =
            Address::from_str(&taker_address).unwrap().assume_checked();

        taker_cli
            .bitcoind
            .client
            .send_to_address(
                &taker_address,
                Amount::ONE_BTC,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
    }

    // confirm balance
    taker_cli.generate_blocks(10);

    // Assert that total_balance & seed_balance must be 3 BTC
    let seed_balance = taker_cli.execute(&["seed-balance"]);
    let total_balance = taker_cli.execute(&["total-balance"]);

    assert_eq!("300000000 SAT", seed_balance);
    assert_eq!("300000000 SAT", total_balance);

    // Assert that total no of seed-utxos are 3.
    let seed_utxos = taker_cli.execute(&["seed-utxo"]);

    let no_of_seed_utxos = seed_utxos.matches("ListUnspentResultEntry {").count();
    assert_eq!(3, no_of_seed_utxos);

    // Send 100,000 sats to a new address within the wallet, with a fee of 1,000 sats.

    // get new external address
    let new_address = taker_cli.execute(&["get-new-address"]);

    let response = taker_cli.execute(&["send-to-address", &new_address, "100000", "1000"]);

    // Extract Transaction hex string
    let tx_hex_start = response.find("transaction_hex").unwrap() + "transaction_hex :  \"".len();
    let tx_hex_end = response.find("\"\n").unwrap();

    let tx_hex = &response[tx_hex_start..tx_hex_end];

    // Extract FeeRate
    let fee_rate_start = response.find("FeeRate").unwrap() + "FeeRate : ".len();
    let fee_rate_end = response.find(" sat").unwrap();

    let _fee_rate = &response[fee_rate_start..fee_rate_end];
    // TODO: Determine if asserts are needed for the calculated fee rate.

    let tx: Transaction = bitcoin::consensus::encode::deserialize_hex(tx_hex).unwrap();

    // broadcast signed transaction
    taker_cli.bitcoind.client.send_raw_transaction(&tx).unwrap();

    taker_cli.generate_blocks(10);

    // Assert the total_amount & seed_amount must be initial (balance -fee)
    let seed_balance = taker_cli.execute(&["seed-balance"]);
    let total_balance = taker_cli.execute(&["total-balance"]);

    // Since the amount is sent back to our wallet, the transaction fee is deducted from the balance.
    assert_eq!("299999000 SAT", seed_balance);
    assert_eq!("299999000 SAT", total_balance);

    // Assert that no of seed utxos are 2
    let seed_utxos = taker_cli.execute(&["seed-utxo"]);

    let no_of_seed_utxos = seed_utxos.matches("ListUnspentResultEntry {").count();
    assert_eq!(4, no_of_seed_utxos);

    taker_cli.bitcoind.client.stop().unwrap();

    // Wait for some time for successfull shutdown of bitcoind.
    std::thread::sleep(std::time::Duration::from_secs(3));
}

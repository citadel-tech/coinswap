#![cfg(feature = "integration-test")]
use bitcoin::{address::NetworkChecked, Address, Amount, Transaction};
use bitcoind::{bitcoincore_rpc::RpcApi, tempfile::env::temp_dir, BitcoinD};

use std::{fs, path::PathBuf, process::Command, str::FromStr};
mod test_framework;
use test_framework::{generate_blocks, init_bitcoind, send_to_address};
/// The taker-cli command struct
struct TakerCli {
    data_dir: PathBuf,
    bitcoind: BitcoinD,
}

impl TakerCli {
    /// Construct a new [`TakerCli`] struct that also include initiating bitcoind.
    fn new() -> TakerCli {
        // Initiate the bitcoind backend.

        let temp_dir = temp_dir().join("coinswap");

        // Remove if previously existing
        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir).unwrap();
        }

        let bitcoind = init_bitcoind(&temp_dir);
        let data_dir = temp_dir.join("taker");

        TakerCli { data_dir, bitcoind }
    }

    // Execute a cli-command
    fn execute(&self, cmd: &[&str]) -> String {
        let mut args = vec![
            "--data-directory",
            self.data_dir.to_str().unwrap(),
            "--bitcoin-network",
            "regtest",
            "--connection-type",
            "clearnet",
        ];

        // RPC authentication (user:password) from the cookie file
        let cookie_file_path = &self.bitcoind.params.cookie_file;
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
}

#[test]
fn test_taker_cli() {
    let taker_cli = TakerCli::new();

    let bitcoind = &taker_cli.bitcoind;
    // Fund the taker with 3 utxos of 1 BTC each.
    for _ in 0..3 {
        let taker_address = taker_cli.execute(&["get-new-address"]);

        let taker_address: Address<NetworkChecked> =
            Address::from_str(&taker_address).unwrap().assume_checked();

        send_to_address(bitcoind, &taker_address, Amount::ONE_BTC);
    }

    // confirm balance
    generate_blocks(bitcoind, 10);

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
    bitcoind.client.send_raw_transaction(&tx).unwrap();

    generate_blocks(bitcoind, 10);

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

    bitcoind.client.stop().unwrap();

    // Wait for some time for successfull shutdown of bitcoind.
    std::thread::sleep(std::time::Duration::from_secs(3));
}

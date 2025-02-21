#![cfg(feature = "integration-test")]
use bitcoin::{address::NetworkChecked, Address, Amount};
use bitcoind::{bitcoincore_rpc::RpcApi, tempfile::env::temp_dir, BitcoinD};

use serde_json::Value;
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
        let mut args = vec!["--data-directory", self.data_dir.to_str().unwrap()];

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

        for arg in cmd {
            args.push(arg);
        }

        let output = Command::new(env!("CARGO_BIN_EXE_taker"))
            .args(args)
            .output()
            .expect("Failed to execute taker");

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
    let balances = taker_cli.execute(&["get-balances"]);
    let balances = serde_json::from_str::<Value>(&balances).unwrap();

    assert_eq!("300000000", balances["regular"].to_string());
    assert_eq!("0", balances["swap"].to_string());
    assert_eq!("0", balances["contract"].to_string());
    assert_eq!("300000000", balances["spendable"].to_string());

    // Assert that total no of seed-utxos are 3.
    let all_utxos = taker_cli.execute(&["list-utxo"]);

    let no_of_seed_utxos = all_utxos.matches("addr").count();
    assert_eq!(3, no_of_seed_utxos);

    // Send 100,000 sats to a new address within the wallet, with a fee of 1,000 sats.

    // get new external address
    let new_address = taker_cli.execute(&["get-new-address"]);

    let _ = taker_cli.execute(&[
        "send-to-address",
        "-t",
        &new_address,
        "-a",
        "100000",
        "-f",
        "1000",
    ]);

    generate_blocks(bitcoind, 10);

    // Assert the total_amount & seed_amount must be initial (balance -fee)
    let balances = taker_cli.execute(&["get-balances"]);
    let balances = serde_json::from_str::<Value>(&balances).unwrap();

    // Since the amount is sent back to our wallet, the transaction fee is deducted from the balance.
    assert_eq!("299999000", balances["regular"].to_string());
    assert_eq!("0", balances["swap"].to_string());
    assert_eq!("0", balances["contract"].to_string());
    assert_eq!("299999000", balances["spendable"].to_string());

    // Assert that no of seed utxos are 2
    let all_utxos = taker_cli.execute(&["list-utxo"]);

    let no_of_seed_utxos = all_utxos.matches("addr").count();
    assert_eq!(4, no_of_seed_utxos);

    bitcoind.client.stop().unwrap();

    // Wait for some time for successfull shutdown of bitcoind.
    std::thread::sleep(std::time::Duration::from_secs(3));
}

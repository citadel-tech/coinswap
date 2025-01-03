//! Integration test for Maker CLI functionality.
#![cfg(feature = "integration-test")]
use bitcoin::{Address, Amount};
use bitcoind::BitcoinD;
use coinswap::utill::setup_logger;
use std::{
    fs,
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Child, Command},
    str::FromStr,
    sync::mpsc::{self, Receiver},
    thread,
    time::Duration,
};

mod test_framework;
use test_framework::{await_message, generate_blocks, init_bitcoind, send_to_address, start_dns};

struct MakerCli {
    data_dir: PathBuf,
    bitcoind: BitcoinD,
}

impl MakerCli {
    /// Initializes Maker CLI
    fn new() -> Self {
        let temp_dir = std::env::temp_dir().join("coinswap");
        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir).unwrap();
        }
        log::info!("temporary directory : {}", temp_dir.display());

        let bitcoind = init_bitcoind(&temp_dir);

        let data_dir = temp_dir.join("maker");
        fs::create_dir_all(&data_dir).unwrap();

        MakerCli { data_dir, bitcoind }
    }

    /// Starts the maker daemon and returns a receiver for stdout messages and the process handle.
    fn start_makerd(&self) -> (Receiver<String>, Child) {
        let (stdout_sender, stdout_recv) = mpsc::channel();
        let (stderr_sender, stderr_recv) = mpsc::channel();

        let rpc_auth = fs::read_to_string(&self.bitcoind.params.cookie_file).unwrap();
        let rpc_address = self.bitcoind.params.rpc_socket.to_string();

        let mut makerd_process = Command::new("./target/debug/makerd")
            .args([
                "--data-directory",
                self.data_dir.to_str().unwrap(),
                "-a",
                &rpc_auth,
                "-r",
                &rpc_address,
                "-w",
                "maker-wallet",
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();

        let stdout = makerd_process.stdout.take().unwrap();
        let stderr = makerd_process.stderr.take().unwrap();

        // Spawn threads to capture stdout and stderr.
        thread::spawn(move || {
            let reader = BufReader::new(stderr);
            if let Some(line) = reader.lines().map_while(Result::ok).next() {
                println!("{}", line);
                stderr_sender.send(line).unwrap();
            }
        });

        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                println!("{}", line);
                if stdout_sender.send(line).is_err() {
                    break;
                }
            }
        });

        // Check for early errors.
        if let Ok(stderr) = stderr_recv.recv_timeout(Duration::from_secs(10)) {
            panic!("Error: {:?}", stderr)
        }

        let (amount, addrs) = loop {
            let log_message = stdout_recv.recv().unwrap();
            if log_message.contains("Send at least 0.05001000 BTC") {
                let parts: Vec<&str> = log_message.split_whitespace().collect();
                let amount = Amount::from_str_in(parts[7], bitcoin::Denomination::Bitcoin).unwrap();
                let addr = Address::from_str(parts[10]).unwrap().assume_checked();
                break (amount, addr);
            }
        };

        // Fund the maker wallet.
        let funding_txid = send_to_address(
            &self.bitcoind,
            &addrs,
            amount.checked_add(Amount::from_btc(0.01).unwrap()).unwrap(),
        );
        log::info!("Sent the Funding Tx: {}", funding_txid);

        // Confirm transactions and setup.
        await_message(&stdout_recv, "Fidelity Transaction");
        generate_blocks(&self.bitcoind, 1);
        await_message(&stdout_recv, "Successfully created fidelity bond");
        await_message(&stdout_recv, "Server Setup completed!!");

        (stdout_recv, makerd_process)
    }

    /// Executes the maker CLI command with given arguments and returns the output.
    fn execute_maker_cli(&self, args: &[&str]) -> String {
        let output = Command::new("./target/debug/maker-cli")
            .args(args)
            .output()
            .unwrap();

        let mut value = output.stdout;
        let error = output.stderr;

        if !error.is_empty() {
            panic!("Error: {:?}", String::from_utf8(error).unwrap());
        }

        value.pop(); // Remove trailing newline.

        std::str::from_utf8(&value).unwrap().to_string()
    }
}

#[test]
fn test_maker_cli() {
    setup_logger(log::LevelFilter::Info);

    let maker_cli = MakerCli::new();

    let dns_dir = maker_cli.data_dir.parent().unwrap();
    let mut directoryd_proc = start_dns(dns_dir, &maker_cli.bitcoind);
    let (rx, mut makerd_proc) = maker_cli.start_makerd();

    // Ping check
    let ping_resp = maker_cli.execute_maker_cli(&["send-ping"]);
    await_message(&rx, "RPC request received: Ping");
    assert_eq!(ping_resp, "success");

    // Data Dir check
    let data_dir = maker_cli.execute_maker_cli(&["show-data-dir"]);
    await_message(&rx, "RPC request received: GetDataDir");
    assert!(data_dir.contains("/coinswap/maker"));

    // // Tor address check
    // let tor_addr = maker_cli.execute_maker_cli(&["show-tor-address"]);
    // await_message(&rx, "RPC request received: GetTorAddress");
    // assert!(tor_addr.contains("onion:6102"));

    // Initial Balance checks
    let seed_balance = maker_cli.execute_maker_cli(&["get-balance"]);
    await_message(&rx, "RPC request received: Balance");

    let contract_balance = maker_cli.execute_maker_cli(&["get-balance-contract"]);
    await_message(&rx, "RPC request received: ContractBalance");

    let fidelity_balance = maker_cli.execute_maker_cli(&["get-balance-fidelity"]);
    await_message(&rx, "RPC request received: FidelityBalance");

    let swap_balance = maker_cli.execute_maker_cli(&["get-balance-swap"]);
    await_message(&rx, "RPC request received: SwapBalance");

    assert_eq!(seed_balance, "1000000 sats");
    assert_eq!(swap_balance, "0 sats");
    assert_eq!(fidelity_balance, "5000000 sats");
    assert_eq!(contract_balance, "0 sats");

    // Initial UTXO checks
    let all_utxos = maker_cli.execute_maker_cli(&["list-utxo"]);
    await_message(&rx, "RPC request received: Utxo");

    let swap_utxo = maker_cli.execute_maker_cli(&["list-utxo-swap"]);
    await_message(&rx, "RPC request received: SwapUtxo");

    let contract_utxo = maker_cli.execute_maker_cli(&["list-utxo-contract"]);
    await_message(&rx, "RPC request received: ContractUtxo");

    let fidelity_utxo = maker_cli.execute_maker_cli(&["list-utxo-fidelity"]);
    await_message(&rx, "RPC request received: FidelityUtxo");

    // Validate UTXOs
    assert_eq!(all_utxos.matches("ListUnspentResultEntry").count(), 2);
    assert!(all_utxos.contains("amount: 1000000 SAT"));
    assert_eq!(fidelity_utxo.matches("ListUnspentResultEntry").count(), 1);
    assert!(fidelity_utxo.contains("amount: 5000000 SAT"));
    assert_eq!(swap_utxo.matches("ListUnspentResultEntry").count(), 0);
    assert_eq!(contract_utxo.matches("ListUnspentResultEntry").count(), 0);

    // Address check - derive and send to address ->
    let address = maker_cli.execute_maker_cli(&["get-new-address"]);
    await_message(&rx, "RPC request received: NewAddress");
    assert!(Address::from_str(&address).is_ok());

    let _ = maker_cli.execute_maker_cli(&[
        "send-to-address",
        "-t",
        &address,
        "-a",
        "10000",
        "-f",
        "1000",
    ]);
    generate_blocks(&maker_cli.bitcoind, 1);

    // Check balances
    assert_eq!(maker_cli.execute_maker_cli(&["get-balance"]), "999000 sats");
    assert_eq!(
        maker_cli.execute_maker_cli(&["get-balance-contract"]),
        "0 sats"
    );
    assert_eq!(
        maker_cli.execute_maker_cli(&["get-balance-fidelity"]),
        "5000000 sats"
    );
    assert_eq!(maker_cli.execute_maker_cli(&["get-balance-swap"]), "0 sats");

    // Verify the seed UTXO count; other balance types remain unaffected when sending funds to an address.
    let seed_utxo = maker_cli.execute_maker_cli(&["list-utxo"]);
    assert_eq!(seed_utxo.matches("ListUnspentResultEntry").count(), 3);

    // Shutdown check
    let stop = maker_cli.execute_maker_cli(&["stop"]);
    await_message(&rx, "RPC request received: Stop");
    assert_eq!(stop, "Shutdown Initiated");

    await_message(&rx, "Maker is shutting down");
    await_message(&rx, "Maker Server is shut down successfully");

    makerd_proc.wait().unwrap();

    directoryd_proc.kill().unwrap();
    directoryd_proc.wait().unwrap();
}

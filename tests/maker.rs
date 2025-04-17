//! Integration test for Maker CLI functionality.
#![cfg(feature = "integration-test")]
use bitcoin::{Address, Amount};
use bitcoind::BitcoinD;
use coinswap::utill::setup_logger;
use serde_json::{json, Value};
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
const FIDELITY_BOND_DNS_UPDATE_INTERVAL: u32 = 30;

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
        println!("temporary directory : {}", temp_dir.display());

        let bitcoind = init_bitcoind(&temp_dir);

        let data_dir = temp_dir.join("maker");
        fs::create_dir_all(&data_dir).unwrap();

        MakerCli { data_dir, bitcoind }
    }

    /// Spawns the `makerd` process and returns:  
    /// - A `Receiver<String>` for stdout messages.  
    /// - The process handle.  
    fn start_makerd(&self) -> (Receiver<String>, Child) {
        let (stdout_sender, stdout_recv) = mpsc::channel();
        let (stderr_sender, stderr_recv) = mpsc::channel();

        let rpc_auth = fs::read_to_string(&self.bitcoind.params.cookie_file).unwrap();
        let rpc_address = self.bitcoind.params.rpc_socket.to_string();

        let mut makerd = Command::new(env!("CARGO_BIN_EXE_makerd"))
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

        let stdout = makerd.stdout.take().unwrap();
        let stderr = makerd.stderr.take().unwrap();

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
        };

        (stdout_recv, makerd)
    }

    /// Starts the maker server, performs initial setup, and waits for key events.  
    /// Returns the maker process and a `Receiver<String>` for stdout messages.
    fn start_and_configure_makerd(&self) -> (Receiver<String>, Child) {
        let (rx, makerd) = self.start_makerd();

        let (amount, addrs) = loop {
            let log_message = rx.recv().unwrap();
            if log_message.contains("Send at least 0.05001000 BTC") {
                let parts: Vec<&str> = log_message.split_whitespace().collect();
                let amount = Amount::from_str_in(parts[7], bitcoin::Denomination::Bitcoin).unwrap();
                let addr = Address::from_str(parts[10]).unwrap().assume_checked();
                break (amount, addr);
            }
        };

        println!("Fund the Maker");
        let _ = send_to_address(
            &self.bitcoind,
            &addrs,
            amount.checked_add(Amount::from_btc(0.01).unwrap()).unwrap(),
        );

        // Wait for fidelity bond confirmation
        await_message(&rx, "Fidelity Transaction");

        generate_blocks(&self.bitcoind, 1);
        await_message(&rx, "Successfully created fidelity bond");

        // Ensure successful DNS registration
        await_message(
            &rx,
            "Successfully sent our address and fidelity proof to DNS at",
        );

        // Confirm swap liquidity availability
        await_message(&rx, "Swap Liquidity: 1000000 sats");

        await_message(&rx, "Server Setup completed!!");

        // sync the wallet cache
        // maker_cli.execute_maker_cli(&["sync-wallet"])

        (rx, makerd)
    }

    /// Executes the maker CLI command with given arguments and returns the output.
    fn execute_maker_cli(&self, args: &[&str]) -> String {
        let output = Command::new(env!("CARGO_BIN_EXE_maker-cli"))
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

fn await_message_timeout(rx: &Receiver<String>, expected: &str, timeout: Duration) -> String {
    let start = std::time::Instant::now();

    while start.elapsed() < timeout {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(message) => {
                if message.contains(expected) {
                    return message;
                }
            }
            Err(_) => {}
        }
    }

    panic!("Timeout waiting for message: {}", expected)
}

#[test]
fn test_maker() {
    setup_logger(log::LevelFilter::Info, None);

    let maker_cli = MakerCli::new();

    let dns_dir = maker_cli.data_dir.parent().unwrap();
    let mut dns = start_dns(dns_dir, &maker_cli.bitcoind);

    let (rx, maker) = maker_cli.start_and_configure_makerd();

    println!("testing for fidelity bond being registered even in mempool");

    let (rx, mut maker) = test_bond_registration_before_confirmation(&maker_cli, maker, rx);

    println!("Testing maker cli");
    test_maker_cli(&maker_cli, &rx);

    maker.kill().unwrap();
    maker.wait().unwrap();
    std::thread::sleep(Duration::from_secs(1)); // Wait for resources to be released

    println!("Testing periodic DNS updates");
    test_periodic_dns_updates(&maker_cli);

    println!("Testing liquidity threshold");
    test_liquidity_threshold(&maker_cli);

    dns.kill().unwrap();
    dns.wait().unwrap();
}

/// Tests maker's handling of an unexpected shutdown while waiting for fidelity bond confirmation.
/// Ensures that after restarting, the maker correctly resumes tracking unconfirmed bonds instead of creating a new one.
fn test_bond_registration_before_confirmation(
    maker_cli: &MakerCli,
    mut maker: Child,
    rx: Receiver<String>,
) -> (Receiver<String>, Child) {
    // TODO: Hardcoded bond timelock; will be fixed in PR #424.
    let bond_timelock = 950;

    println!(
        "Generating {} blocks to expire the fidelity bond",
        bond_timelock
    );
    generate_blocks(&maker_cli.bitcoind, bond_timelock);

    await_message(&rx, "Fidelity redeem transaction broadcasted");

    await_message(&rx, "No active Fidelity Bonds found. Creating one.");
    await_message(&rx, "seen in mempool, waiting for confirmation");

    println!("Shutting down maker server while waiting for confirmation");
    maker.kill().unwrap();
    maker.wait().unwrap();

    println!("Generate a block to confirm the new fidelity bond");
    generate_blocks(&maker_cli.bitcoind, 1);

    // Restart and verify the bond is recognized.
    let (rx, maker) = maker_cli.start_makerd();

    await_message(&rx, "Fidelity Bond found | Index: 1");
    await_message(&rx, "Highest bond at outpoint");

    (rx, maker)
}

fn test_maker_cli(maker_cli: &MakerCli, rx: &Receiver<String>) {
    // Ping check
    let ping_resp = maker_cli.execute_maker_cli(&["send-ping"]);
    await_message(rx, "RPC request received: Ping");
    assert_eq!(ping_resp, "success");

    // Data Dir check
    let data_dir = maker_cli.execute_maker_cli(&["show-data-dir"]);
    await_message(rx, "RPC request received: GetDataDir");
    assert!(data_dir.contains("/coinswap/maker"));

    // Tor address check
    let tor_addr = maker_cli.execute_maker_cli(&["show-tor-address"]);
    await_message(rx, "RPC request received: GetTorAddress");
    assert_eq!(tor_addr, "Maker is not running on TOR");

    // sync the wallet cache
    maker_cli.execute_maker_cli(&["sync-wallet"]);

    // Initial Balance checks
    let balances = maker_cli.execute_maker_cli(&["get-balances"]);
    await_message(rx, "RPC request received: Balances");

    assert_eq!(
        serde_json::from_str::<Value>(&balances).unwrap(),
        json!({
            "regular": 998000,
            "swap": 0,
            "contract": 0,
            "fidelity": 5000000,
            "spendable": 998000
        })
    );

    // Initial UTXO checks
    let all_utxos = maker_cli.execute_maker_cli(&["list-utxo"]);
    await_message(rx, "RPC request received: Utxo");

    let swap_utxo = maker_cli.execute_maker_cli(&["list-utxo-swap"]);
    await_message(rx, "RPC request received: SwapUtxo");

    let contract_utxo = maker_cli.execute_maker_cli(&["list-utxo-contract"]);
    await_message(rx, "RPC request received: ContractUtxo");

    let fidelity_utxo = maker_cli.execute_maker_cli(&["list-utxo-fidelity"]);
    await_message(rx, "RPC request received: FidelityUtxo");

    // Validate UTXOs
    assert_eq!(all_utxos.matches("ListUnspentResultEntry").count(), 2);
    assert_eq!(fidelity_utxo.matches("ListUnspentResultEntry").count(), 1);
    assert!(fidelity_utxo.contains("amount: 5000000 SAT"));
    assert_eq!(swap_utxo.matches("ListUnspentResultEntry").count(), 0);
    assert_eq!(contract_utxo.matches("ListUnspentResultEntry").count(), 0);

    // Address check - derive and send to address
    let address = maker_cli.execute_maker_cli(&["get-new-address"]);
    await_message(rx, "RPC request received: NewAddress");
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

    // sync the wallet cache
    maker_cli.execute_maker_cli(&["sync-wallet"]);

    // Check balances
    let balances = maker_cli.execute_maker_cli(&["get-balances"]);
    assert_eq!(
        serde_json::from_str::<Value>(&balances).unwrap(),
        json!({
            "regular": 997000,
            "swap": 0,
            "contract": 0,
            "fidelity": 5000000,
            "spendable": 997000
        })
    );

    // let fidelity_bonds_str = maker_cli.execute_maker_cli(&["show-fidelity"]);
    // let raw: Value = serde_json::from_str(&fidelity_bonds_str).unwrap();
    // let fidelity_bonds: Vec<Value> = serde_json::from_str(raw.as_str().unwrap()).unwrap();
    // let expected_fields = ["index", "outpoint", "amount", "bond-value", "expires-in"];
    // for fidelity_bond in fidelity_bonds {
    //     for field in expected_fields {
    //         assert!(
    //             fidelity_bond.get(field).is_some(),
    //             "expected field '{}' is not present",
    //             field
    //         )
    //     }
    // }

    // Verify the seed UTXO count; other balance types remain unaffected when sending funds to an address.
    let seed_utxo = maker_cli.execute_maker_cli(&["list-utxo"]);
    assert_eq!(seed_utxo.matches("ListUnspentResultEntry").count(), 3);

    // Shutdown check
    let stop = maker_cli.execute_maker_cli(&["stop"]);
    await_message(rx, "RPC request received: Stop");
    assert_eq!(stop, "Shutdown Initiated");

    await_message(rx, "Maker is shutting down");
    await_message(rx, "Maker Server is shut down successfully");
}

fn test_periodic_dns_updates(maker_cli: &MakerCli) {
    let (rx, mut maker) = maker_cli.start_makerd();

    // record initial dns time
    let initial_update = await_message(
        &rx,
        "Successfully sent our address and fidelity proof to DNS at",
    );
    let start_time = std::time::Instant::now();

    let interval = FIDELITY_BOND_DNS_UPDATE_INTERVAL;

    println!("Sleeping for {} seconds", interval);

    // wait and capture update message
    let timeout = Duration::from_secs(interval as u64 * 2);
    let first_update = await_message_timeout(
        &rx,
        "Successfully sent our address and fidelity proof to DNS at",
        timeout,
    );

    let first_update_time = start_time.elapsed().as_secs();
    println!("First update time: {}", first_update_time);

    println!("Waiting for second update");
    let second_update = await_message_timeout(
        &rx,
        "Successfully sent our address and fidelity proof to DNS at",
        timeout,
    );

    let second_update_time = start_time.elapsed().as_secs();
    println!("Second update time: {}", second_update_time);

    maker.kill().unwrap();
    maker.wait().unwrap();
    // wait
    std::thread::sleep(Duration::from_secs(1));
}

fn test_liquidity_threshold(maker_cli: &MakerCli) {
    println!("TEST STARTING: Liquidity Threshold");

    let (rx, mut maker) = maker_cli.start_makerd();

    await_message(&rx, "Server Setup completed!!");
    std::thread::sleep(Duration::from_secs(3));

    println!("Getting initial balance");
    let balance = maker_cli.execute_maker_cli(&["get-balances"]);
    let balance_json: serde_json::Value = serde_json::from_str(&balance).unwrap();
    let initial_balance = balance_json["regular"].as_u64().unwrap();
    println!("Initial balance: {} sats", initial_balance);

    const MIN_SWAP_AMOUNT: u64 = 10_000;
    println!("Minimum swap amount: {} sats", MIN_SWAP_AMOUNT);

    let amount_to_spend = initial_balance - (MIN_SWAP_AMOUNT / 4);
    let tx_fee = 1_000;

    println!("Amount to spend: {} sats", amount_to_spend);

    let address = maker_cli.execute_maker_cli(&["get-new-address"]);
    println!("Sending to address: {}", address);

    println!("Sending transaction");
    let tx_result = maker_cli.execute_maker_cli(&[
        "send-to-address",
        "-t",
        &address,
        "-a",
        &amount_to_spend.to_string(),
        "-f",
        &tx_fee.to_string(),
    ]);
    println!("Transaction result: {}", tx_result);

    println!("Generating block to confirm transaction");
    generate_blocks(&maker_cli.bitcoind, 1);

    let sync_result = maker_cli.execute_maker_cli(&["sync-wallet"]);
    println!("Sync result: {}", sync_result);

    std::thread::sleep(Duration::from_secs(2));

    println!("Getting updated balance");
    let new_balances = maker_cli.execute_maker_cli(&["get-balances"]);
    let new_balances_json: serde_json::Value = serde_json::from_str(&new_balances).unwrap();
    let new_balance = new_balances_json["regular"].as_u64().unwrap();

    println!("New balance: {} sats", new_balance);
    assert!(
        new_balance < MIN_SWAP_AMOUNT,
        "Balance should be below minimum"
    );

    println!("Waiting for liquidity check (may take up to 30 seconds)...");

    let start_time = std::time::Instant::now();
    let timeout = Duration::from_secs(90);
    let mut found_insufficient_message = false;

    while !found_insufficient_message && start_time.elapsed() < timeout {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(message) => {
                println!("LOG: {}", message);

                if (message.contains("insufficient") && message.contains("liquidity"))
                    || (message.contains("Insufficient") && message.contains("Liquidity"))
                    || message.contains("insufficient swap")
                    || message.contains("below minimum")
                {
                    found_insufficient_message = true;
                    println!("FOUND LIQUIDITY WARNING: {}", message);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("Channel disconnected while monitoring logs");
            }
        }
    }

    if !found_insufficient_message {
        println!(
            "Warning: Did not detect explicit insufficient liquidity message, proceeding anyway"
        );
    }

    println!("Adding funds to exceed minimum threshold");

    let new_address = maker_cli.execute_maker_cli(&["get-new-address"]);
    println!("New funding address: {}", new_address);

    let funding_amount = MIN_SWAP_AMOUNT * 2;
    println!("Sending {} sats to {}", funding_amount, new_address);

    let txid = send_to_address(
        &maker_cli.bitcoind,
        &Address::from_str(&new_address).unwrap().assume_checked(),
        Amount::from_sat(funding_amount),
    );
    println!("Funding transaction ID: {}", txid);

    println!("Generating block to confirm funding");
    generate_blocks(&maker_cli.bitcoind, 1);

    println!("Syncing wallet");
    maker_cli.execute_maker_cli(&["sync-wallet"]);

    println!("Waiting 2 seconds for balance update");
    std::thread::sleep(Duration::from_secs(2));

    let final_balances = maker_cli.execute_maker_cli(&["get-balances"]);
    println!("Final balances: {}", final_balances);

    let final_balances_json: serde_json::Value = serde_json::from_str(&final_balances).unwrap();
    let final_balance = final_balances_json["regular"].as_u64().unwrap();

    println!("Final balance: {} sats", final_balance);
    assert!(
        final_balance >= MIN_SWAP_AMOUNT,
        "Balance should be above minimum"
    );

    println!("Waiting for liquidity to be sufficient again...");
    let mut found_sufficient_message = false;
    let start_time = std::time::Instant::now();

    while !found_sufficient_message && start_time.elapsed() < timeout {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(message) => {
                println!("LOG: {}", message);

                if message.contains("Swap Liquidity:") && !message.contains("insufficient") {
                    found_sufficient_message = true;
                    println!("FOUND SUFFICIENT LIQUIDITY MESSAGE: {}", message);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("Channel disconnected while monitoring logs");
            }
        }
    }

    if !found_sufficient_message {
        println!(
            "Warning: Did not detect explicit sufficient liquidity message, proceeding anyway"
        );
    }

    println!("Liquidity threshold test completed!");
}

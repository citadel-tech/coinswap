#![cfg(feature = "integration-test")]
use bitcoin::{Address, Amount, Network};
use bitcoind::{bitcoincore_rpc::RpcApi, BitcoinD};
use coinswap::utill::setup_logger;
use std::{
    fs,
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Child, Command},
    str::FromStr,
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::Duration,
};

mod test_framework;
use test_framework::{get_random_tmp_dir, init_bitcoind};

struct MakerCli {
    data_dir: PathBuf,
    bitcoind: BitcoinD,
}

impl MakerCli {
    fn new() -> Self {
        setup_logger(log::LevelFilter::Info);
        // Setup directory
        let temp_dir = get_random_tmp_dir();
        // Remove if previously existing
        if temp_dir.exists() {
            fs::remove_dir_all::<PathBuf>(temp_dir.clone()).unwrap();
        }
        log::info!("temporary directory : {}", temp_dir.display());

        let bitcoind = init_bitcoind(&temp_dir);

        let mining_address = bitcoind
            .client
            .get_new_address(None, None)
            .unwrap()
            .require_network(Network::Regtest)
            .unwrap();
        bitcoind
            .client
            .generate_to_address(101, &mining_address)
            .unwrap();

        let data_dir = temp_dir.join("maker");
        fs::create_dir_all(&data_dir).unwrap();

        MakerCli { data_dir, bitcoind }
    }

    fn start_makerd(&self) -> (Receiver<String>, Child, Child) {
        let (log_sender, log_receiver): (Sender<String>, Receiver<String>) = mpsc::channel();

        let mut directoryd_process = Command::new("./target/debug/directoryd")
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();

        let stdout = directoryd_process.stdout.take().unwrap();
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            reader.lines().map_while(Result::ok).for_each(|line| {
                println!("{}", line);
                log_sender.send(line).unwrap_or_else(|e| {
                    println!("Failed to send log: {}", e);
                });
            });
        });

        while let Ok(log_message) = log_receiver.recv_timeout(Duration::from_secs(5)) {
            if log_message.contains("RPC socket binding successful") {
                println!("DNS Started");
                break;
            }
        }

        log::info!("DNS Server started");

        let (maker_log_sender, maker_log_recvr) = mpsc::channel();
        let data_dir = self.data_dir.clone();

        let cookie_file_path = self.bitcoind.params.cookie_file.clone();
        let rpc_auth = fs::read_to_string(cookie_file_path).expect("failed to read from file");
        let rpc_address = self.bitcoind.params.rpc_socket.to_string();

        let mut makerd_process = Command::new("./target/debug/makerd")
            .args([
                "--data-directory",
                data_dir.to_str().unwrap(),
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

        log::info!("Maker Server started");

        let stdout = makerd_process.stdout.take().unwrap();
        let stderr = makerd_process.stderr.take().unwrap();
        let sender = maker_log_sender.clone();
        // start the thread to get the logs
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            reader.lines().map_while(Result::ok).for_each(|line| {
                println!("{}", line);
                sender.send(line).unwrap_or_else(|e| {
                    println!("Failed to send log: {}", e);
                });
            });
        });

        // Panic if anything is found in std error.
        thread::spawn(move || {
            let reader = BufReader::new(stderr);
            reader.lines().map_while(Result::ok).for_each(|line| {
                panic!("Error: {}", line);
            });
        });

        let (amount, addrs) = loop {
            let log_message = maker_log_recvr.recv().unwrap();
            if log_message.contains("Send at least 0.05001000 BTC") {
                let parts: Vec<&str> = log_message.split_whitespace().collect();
                let amount = Amount::from_str_in(parts[7], bitcoin::Denomination::Bitcoin).unwrap();
                let addr = Address::from_str(parts[10]).unwrap().assume_checked(); // Do it properly
                break (amount, addr);
            } else {
                println!("Waiting for fidelity initialization.")
            }
        };

        // Fund the fidelity
        let fidelity_txid = self
            .bitcoind
            .client
            .send_to_address(
                &addrs,
                amount.checked_add(Amount::from_btc(0.01).unwrap()).unwrap(),
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();

        log::info!("Sent the Funding Tx: {}", fidelity_txid);

        // Wait for mempool
        loop {
            let log_message = maker_log_recvr.recv().unwrap();
            if log_message.contains("seen in mempool, waiting for confirmation") {
                break;
            }
        }

        log::info!("Confirming the fidelity tx");

        // Wait for confirmation
        let mining_address = self
            .bitcoind
            .client
            .get_new_address(None, None)
            .unwrap()
            .require_network(Network::Regtest)
            .unwrap();

        self.bitcoind
            .client
            .generate_to_address(1, &mining_address)
            .unwrap();

        // Wait final setup
        loop {
            let log_message = maker_log_recvr.recv().unwrap();
            if log_message.contains("Maker setup is ready") {
                break;
            }
        }

        log::info!("Maker setup is ready");

        (maker_log_recvr, makerd_process, directoryd_process)
    }

    fn execute_maker_cli(&self, args: &[&str]) -> String {
        let output = Command::new("./target/debug/maker-cli")
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
fn test_makecli_get_new_address() {
    let maker_cli = MakerCli::new();
    let (rx, mut makerd_proc, mut directoryd_proc) = maker_cli.start_makerd();

    // Address check
    let addr_resp = maker_cli.execute_maker_cli(&["new-address"]);
    loop {
        let log_message = rx.recv().unwrap();
        if log_message.contains(" RPC request received: NewAddress") {
            log::info!("RPC Message received");
            break;
        }
    }
    assert!(Address::from_str(&addr_resp).is_ok());

    // Balance checks
    let seed_balance = maker_cli.execute_maker_cli(&["seed-balance"]);
    loop {
        let log_message = rx.recv().unwrap();
        if log_message.contains(" RPC request received: SeedBalance") {
            log::info!("RPC Message received");
            break;
        }
    }
    let contract_balance = maker_cli.execute_maker_cli(&["contract-balance"]);
    loop {
        let log_message = rx.recv().unwrap();
        if log_message.contains(" RPC request received: ContractBalance") {
            log::info!("RPC Message received");
            break;
        }
    }
    let fidelity_balance = maker_cli.execute_maker_cli(&["fidelity-balance"]);
    loop {
        let log_message = rx.recv().unwrap();
        if log_message.contains(" RPC request received: FidelityBalance") {
            log::info!("RPC Message received");
            break;
        }
    }
    let swap_balance = maker_cli.execute_maker_cli(&["swap-balance"]);
    loop {
        let log_message = rx.recv().unwrap();
        if log_message.contains(" RPC request received: SwapBalance") {
            log::info!("RPC Message received");
            break;
        }
    }
    assert_eq!(seed_balance, "1000000 sats");
    assert_eq!(swap_balance, "0 sats");
    assert_eq!(fidelity_balance, "5000000 sats");
    assert_eq!(contract_balance, "0 sats");

    // UTXO checks
    let seed_utxo = maker_cli.execute_maker_cli(&["seed-utxo"]);
    loop {
        let log_message = rx.recv().unwrap();
        if log_message.contains(" RPC request received: SeedUtxo") {
            log::info!("RPC Message received");
            break;
        }
    }
    let swap_utxo = maker_cli.execute_maker_cli(&["swap-utxo"]);
    loop {
        let log_message = rx.recv().unwrap();
        if log_message.contains(" RPC request received: SwapUtxo") {
            log::info!("RPC Message received");
            break;
        }
    }
    let contract_utxo = maker_cli.execute_maker_cli(&["contract-utxo"]);
    loop {
        let log_message = rx.recv().unwrap();
        if log_message.contains(" RPC request received: ContractUtxo") {
            log::info!("RPC Message received");
            break;
        }
    }
    let fidelity_utxo = maker_cli.execute_maker_cli(&["fidelity-utxo"]);
    loop {
        let log_message = rx.recv().unwrap();
        if log_message.contains(" RPC request received: FidelityUtxo") {
            log::info!("RPC Message received");
            break;
        }
    }

    assert_eq!(seed_utxo.matches("ListUnspentResultEntry").count(), 1);
    assert!(seed_utxo.contains("amount: 1000000 SAT"));
    assert_eq!(fidelity_utxo.matches("ListUnspentResultEntry").count(), 1);
    assert!(fidelity_utxo.contains("amount: 5000000 SAT"));
    assert_eq!(swap_utxo.matches("ListUnspentResultEntry").count(), 0);
    assert_eq!(contract_utxo.matches("ListUnspentResultEntry").count(), 0);

    let data_dir = maker_cli.execute_maker_cli(&["get-data-dir"]);
    let tor_addr = maker_cli.execute_maker_cli(&["get-tor-address"]);

    assert!(data_dir.contains("/tmp/.coinswap/"));
    assert!(tor_addr.contains(".onion:6102"));

    // Stop everything
    let stop = maker_cli.execute_maker_cli(&["stop"]);
    loop {
        let log_message = rx.recv().unwrap();
        if log_message.contains(" RPC request received: Stop") {
            log::info!("RPC Message received");
            break;
        }
    }
    assert_eq!(stop, "Shutdown Initiated");
    directoryd_proc.kill().unwrap();
    directoryd_proc.wait().unwrap();
    makerd_proc.wait().unwrap();
}

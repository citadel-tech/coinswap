use std::{
    env::temp_dir,
    fs,
    io::{BufRead, BufReader, Write},
    net::TcpStream,
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::Duration,
};

use bitcoind::{bitcoincore_rpc::RpcApi, BitcoinD, Conf};
use coinswap::protocol::messages::FidelityProof;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
struct OfferMetadata {
    url: String,
    proof: FidelityProof,
}

// Structured requests and responses using serde.
#[derive(Serialize, Deserialize, Debug)]
enum Request {
    Post { metadata: Box<OfferMetadata> },
    Get { makers: u32 },
    Dummy { url: String },
}

fn start_server(bitcoin_process: &BitcoinProcess) -> (Child, Receiver<String>) {
    let mut args = vec![
        "--data-directory",
        bitcoin_process.data_dir.to_str().unwrap(),
        "--network",
        "clearnet",
        "--rpc_network",
        "regtest",
    ];

    // RPC authentication (user:password) from the cookie file
    let cookie_file_path = Path::new(&bitcoin_process.bitcoind.params.cookie_file);
    let rpc_auth = fs::read_to_string(cookie_file_path).expect("failed to read from file");
    args.push("--USER:PASSWORD");
    args.push(&rpc_auth);

    // Full node address for RPC connection
    let rpc_address = bitcoin_process.bitcoind.params.rpc_socket.to_string();
    args.push("--ADDRESS:PORT");
    args.push(&rpc_address);

    args.push("--WALLET");
    args.push("test_wallet");
    let (log_sender, log_receiver): (Sender<String>, Receiver<String>) = mpsc::channel();
    let mut directoryd_process = Command::new("./target/debug/directoryd")
        .args(args)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let stdout = directoryd_process.stdout.take().unwrap();
    thread::spawn(move || {
        let reader = BufReader::new(stdout);
        reader.lines().map_while(Result::ok).for_each(|line| {
            log_sender.send(line).unwrap_or_else(|e| {
                println!("Failed to send log: {}", e);
            });
        });
    });

    (directoryd_process, log_receiver)
}

fn wait_for_server_start(log_receiver: &Receiver<String>) {
    let mut server_started = false;
    while let Ok(log_message) = log_receiver.recv_timeout(Duration::from_secs(5)) {
        if log_message.contains("RPC socket binding successful") {
            server_started = true;
            break;
        }
    }
    assert!(
        server_started,
        "Server did not start within the expected time"
    );
}

fn send_addresses(addresses: &[&str]) {
    for address in addresses {
        let mut stream = TcpStream::connect(("127.0.0.1", 8080)).unwrap();
        let request = Request::Dummy {
            url: address.to_string(),
        };
        let buffer = serde_cbor::ser::to_vec(&request).unwrap();
        let length = buffer.len() as u64;
        stream.write_all(&length.to_be_bytes()).unwrap();
        stream.write_all(&buffer).unwrap();
        stream.flush().unwrap();
        // stream.write_all(dummy_payload.).unwrap();
    }
}

fn verify_addresses(addresses: &[&str]) {
    let output = Command::new("./target/debug/directory-cli")
        .arg("list-addresses")
        .output()
        .unwrap();
    let addresses_output = String::from_utf8(output.stdout).unwrap();

    assert!(
        output.stderr.is_empty(),
        "Error: {:?}",
        String::from_utf8(output.stderr).unwrap()
    );

    for address in addresses {
        assert!(
            addresses_output.contains(&address.to_string()),
            "Address {} not found",
            address
        );
    }
}

struct BitcoinProcess {
    data_dir: PathBuf,
    bitcoind: BitcoinD,
}

impl BitcoinProcess {
    /// Construct a new [`TakerCli`] struct that also include initiating bitcoind.
    fn new() -> BitcoinProcess {
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

        let data_dir = temp_dir.join("directory");

        BitcoinProcess { data_dir, bitcoind }
    }
}

#[test]
fn test_dns() {
    let bitcoind_process = BitcoinProcess::new();
    let (mut process, receiver) = start_server(&bitcoind_process);
    wait_for_server_start(&receiver);

    let initial_addresses = vec!["127.0.0.1:8080", "127.0.0.1:8081", "127.0.0.1:8082"];
    send_addresses(&initial_addresses);
    thread::sleep(Duration::from_secs(10));
    verify_addresses(&initial_addresses);

    // Persistence check
    process.kill().expect("Failed to kill directoryd process");
    process.wait().unwrap();

    let (mut process, receiver) = start_server(&bitcoind_process);
    wait_for_server_start(&receiver);

    let additional_addresses = vec!["127.0.0.1:8083", "127.0.0.1:8084"];
    send_addresses(&additional_addresses);
    thread::sleep(Duration::from_secs(10));

    process.kill().expect("Failed to kill directoryd process");
    process.wait().unwrap();

    let (mut process, receiver) = start_server(&bitcoind_process);
    wait_for_server_start(&receiver);

    let all_addresses = vec![
        "127.0.0.1:8080",
        "127.0.0.1:8081",
        "127.0.0.1:8082",
        "127.0.0.1:8083",
        "127.0.0.1:8084",
    ];
    verify_addresses(&all_addresses);

    process.kill().expect("Failed to kill directoryd process");
    process.wait().unwrap();
}

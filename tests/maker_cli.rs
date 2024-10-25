use std::{fs::File, net::Shutdown};
use std::str::FromStr;
use bitcoin::{address::NetworkChecked, Address, Amount, Transaction};
use bitcoind::{
    bitcoincore_rpc::{Auth, RpcApi},
    tempfile::tempdir,
    BitcoinD, Conf, DataDir,
};
use coinswap::{
    maker::{rpc, start_maker_server, Maker, MakerBehavior},
    market::directory::{start_directory_server, DirectoryServer},
    taker::{Taker, TakerBehavior},
    utill::{read_connection_network_string, ConnectionType},
    wallet::RPCConfig,
};

use std::{path::PathBuf, process::Command, sync::{Arc, RwLock}};
// /// Testing errors for integration tests
// #[derive(Debug)]
// enum IntTestError {
//     // IO error
//     IO(std::io::Error),
//     // Command execution error
//     CmdExec(String),
// }

struct MakerCli {
    data_dir: Option<PathBuf>,
    bitcoind: BitcoinD,
    shutdown: Arc<RwLock<bool>>,
}

struct MakerConfig {
    port: u16,
    heart_beat_interval_secs: u64,
    rpc_ping_interval_secs: u64,
    directory_servers_refresh_interval_secs: u64,
    idle_connection_timeout: u64,
    absolute_fee_sats: u64,
    amount_relative_fee_ppb: u64,
    time_relative_fee_ppb: u64,
    required_confirms: u32,
    min_contract_reaction_time: u64,
    min_size: u64,
    socks_part: u16,
    directory_server_onion_address: String,
    connection_type: String,
}

fn parse_maker_toml(file_path: &str) -> Result<MakerConfig, Box<dyn std::error::Error>> {
    let mut file = fs::File::open(file_path)?;
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;

    let config: MakerConfig = toml::from_str(&contents)?;
    Ok(config)
}

fn build_cli_from_config(config: MakerConfig, data_dir: DataDir) -> Cli {
    Cli {
        network: config.connection_type,
        data_directory: Some(DataDir),
        rpc: "127.0.0.1:18443".to_string(), // Default or set dynamically as needed
        auth: ("user".to_string(), "password".to_string()), // Set based on actual auth
        rpc_network: "regtest".to_string(), // Default or from config
        wallet_name: "maker".to_string(), // Default or set dynamically
    }
}

impl MakerCli {
    fn new() -> MakerCli {
        // Initiate bitcoind instance

        let temp_dir = tempdir().unwrap().into_path();
        let mut conf = Conf::default();
        conf.args.push("-txindex=1"); //txindex is must, or else wallet sync won't work.
        conf.staticdir = Some(temp_dir.join(".bitcoin"));

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
        
        // setup directoryd and get the address.
        let data_dir = temp_dir.join("maker");
        let shutdown = Arc::new(RwLock::new(false));
        MakerCli {
            data_dir: Some(data_dir),
            bitcoind,
            shutdown,
        }

    }
    
    fn execute(&self, cmd: &str) -> Result<String> {
        
        // setup the makerd datadir with maker.toml config values. (especially the dns).
        let mut args = vec![
            "--data-directory",
            self.data_dir.as_os_str().to_str().unwrap(),
            "--bitcoin-network",
            "regtest",
            "--connection-type",
            "clearnet",
        ];

        // Gather all the relevant args for makerd and maker-cli. Maybe in two different structs.

        // spawn the makerd thread and get a feed of the log via a mpsc channel. This will give fidelity address:amount to fund.

        // Fund the fidelity address.

        // wait for makerd to complete setup.

        // push  the arguments
        let makers_count = self.makers.len();
        let makers_count_string = makers_count.to_string();

        args.push(&makers_count_string); // makers count
        args.push(&tx_count);

        // add final command
        args.push(cmd);

        let output = Command::new(&self.target).args(args).output().unwrap(); // Return Error

        let mut value = output.stdout;
        let error = output.stderr;
        // Note: The value & error are in bytes format

        if value.len() == 0 {
            return Err(IntTestError::CmdExec(String::from_utf8(error).unwrap()));
        }

        value.pop(); // remove `\n` at end

        // get the output string from bytes
        let output_string = std::str::from_utf8(&value).unwrap().to_string();

        Ok(output_string)
    }

}

fn test_maker_coinswap() {
    let maker_cli = MakerCli::new();

    // Fund the maker with 3 UTXOs of 1 BTC each to provide liquidity for CoinSwap.
    for _ in 0..3 {
        let maker_address = maker_cli.execute(&["get-new-address"]);

        let maker_address: Address<NetworkChecked> =
            Address::from_str(&maker_address).unwrap().assume_checked();

        maker_cli
            .bitcoind
            .client
            .send_to_address(
                &maker_address,
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

    // Confirm the initial balance of the maker by generating blocks.
    maker_cli.generate_blocks(10);

    // Assert that the total balance and seed balance must be 3 BTC.
    let seed_balance = maker_cli.execute(&["seed-balance"]);
    let total_balance = maker_cli.execute(&["total-balance"]);

    assert_eq!("300000000 SAT", seed_balance);
    assert_eq!("300000000 SAT", total_balance);

    // Assert that the total number of seed UTXOs is 3.
    let seed_utxos = maker_cli.execute(&["seed-utxo"]);
    let no_of_seed_utxos = seed_utxos.matches("ListUnspentResultEntry {").count();
    assert_eq!(3, no_of_seed_utxos);

    // Begin a CoinSwap transaction.
    // This simulates a taker requesting to initiate a CoinSwap with the maker.
    let coinswap_init_response = maker_cli.execute(&["initiate-coinswap", "1000000", "1", "10000"]);

    // The maker should provide an output address for the swap.
    // Here we extract the transaction details from the response for further processing.
    let tx_hex_start = coinswap_init_response.find("transaction_hex").unwrap() + "transaction_hex : \"".len();
    let tx_hex_end = coinswap_init_response.find("\"\n").unwrap();
    let tx_hex = &coinswap_init_response[tx_hex_start..tx_hex_end];

    // Deserialize the transaction hex to a Bitcoin Transaction for broadcasting.
    let tx: Transaction = bitcoin::consensus::encode::deserialize_hex(tx_hex).unwrap();

    // The maker broadcasts the CoinSwap transaction.
    maker_cli.bitcoind.client.send_raw_transaction(&tx).unwrap();

    // Generate blocks to confirm the CoinSwap transaction on the blockchain.
    maker_cli.generate_blocks(6); // 6 confirmations to finalize the swap.

    // Confirm the balance after the CoinSwap transaction.
    let seed_balance_after = maker_cli.execute(&["seed-balance"]);
    let total_balance_after = maker_cli.execute(&["total-balance"]);

    // Assert the new balance after the swap. The expected balance would be reduced by the swapped amount.
    assert!(seed_balance_after < "300000000 SAT");
    assert!(total_balance_after < "300000000 SAT");

    // Confirm that a new UTXO was created as part of the swap.
    let seed_utxos_after = maker_cli.execute(&["seed-utxo"]);
    let no_of_seed_utxos_after = seed_utxos_after.matches("ListUnspentResultEntry {").count();
    assert!(no_of_seed_utxos_after > 3);

    // Stop the bitcoind instance cleanly.
    maker_cli.bitcoind.client.stop().unwrap();

    // Wait for successful shutdown of bitcoind.
}

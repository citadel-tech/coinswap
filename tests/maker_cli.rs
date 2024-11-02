#![allow(unused)]
use std::{fs, path::PathBuf, process::Command, sync::{Arc, RwLock}, thread, time::Duration};
use std::str::FromStr;
use bitcoin::{Address, Amount, Network};
use bitcoind::{bitcoincore_rpc::RpcApi, tempfile, BitcoinD, Conf};
use std::sync::mpsc;

struct MakerCli {
    data_dir: PathBuf,
    bitcoind: BitcoinD,
    shutdown: Arc<RwLock<bool>>,
}

impl MakerCli {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?.into_path();
        let mut conf = Conf::default();
        conf.args.push("-txindex=1");
        conf.network = "regtest";

        let bitcoind = BitcoinD::with_conf(bitcoind::exe_path()?, &conf)?;

        let mining_address = bitcoind.client.get_new_address(None, None)?
            .require_network(Network::Regtest)?;
        bitcoind.client.generate_to_address(101, &mining_address)?;

        let data_dir = temp_dir.join("maker");
        fs::create_dir_all(&data_dir)?;

        Ok(MakerCli {
            data_dir,
            bitcoind,
            shutdown: Arc::new(RwLock::new(false)),
        })
    }

    fn start_makerd(&self) -> Result<mpsc::Receiver<String>, Box<dyn std::error::Error>> {
        let (tx, rx) = mpsc::channel();
        let data_dir = self.data_dir.clone();

        thread::spawn(move || {
            let output = Command::new("cargo")
                .args(&[
                    "run",
                    "--bin",
                    "makerd",
                    "--",
                    "--data-directory", data_dir.to_str().unwrap(),
                    "--network", "clearnet",
                    "--rpc", "127.0.0.1:18443",
                    "--auth", "user:password",
                    "--rpc-network", "regtest",
                    "--wallet-name", "maker",
                ])
                .output()
                .expect("Failed to execute makerd");

            let output_str = String::from_utf8_lossy(&output.stdout);
            for line in output_str.lines() {
                tx.send(line.to_string()).unwrap();
            }
        });

        Ok(rx)
    }

    fn wait_for_maker_setup(&self, rx: mpsc::Receiver<String>) -> Result<String, Box<dyn std::error::Error>> {
        for line in rx.iter() {
            if line.contains("Fidelity bond address:") {
                return Ok(line.split(":").last().unwrap().trim().to_string());
            }
        }
        Err("Fidelity bond address not found in makerd output".into())
    }

    fn execute_maker_cli(&self, args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
        let output = Command::new("cargo")
            .args(&["run", "--bin", "maker-cli", "--"])
            .args(args)
            .output()?;

        Ok(String::from_utf8(output.stdout)?.trim().to_string())
    }

    fn execute_directory_cli(&self, args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
        let output = Command::new("cargo")
            .args(&["run", "--bin", "directory-cli", "--"])
            .args(args)
            .output()?;

        Ok(String::from_utf8(output.stdout)?.trim().to_string())
    }
}

#[test]
fn test_makecli_get_new_address() -> Result<(), Box<dyn std::error::Error>> {
    let maker_cli = MakerCli::new()?;
    let rx = maker_cli.start_makerd()?;
    let fidelity_address = maker_cli.wait_for_maker_setup(rx)?;
    let checked_address = Address::from_str(&fidelity_address)?.require_network(Network::Regtest)?;
    
    maker_cli.bitcoind.client.send_to_address(
        &checked_address,
        Amount::from_sat(100_000_000),
        None, None, None, None, None, None
    )?;

    let new_address = maker_cli.bitcoind.client.get_new_address(None, None)?;
    let checked_new_address = Address::assume_checked(new_address);
    maker_cli.bitcoind.client.generate_to_address(6, &checked_new_address)?;

    // Wait for makerd to complete setup
    thread::sleep(Duration::from_secs(10));

    let new_address = maker_cli.execute_maker_cli(&["get-new-address"])?;
    assert!(Address::from_str(&new_address).is_ok());

    let directory_addresses = maker_cli.execute_directory_cli(&["list-addresses"])?;
    assert!(directory_addresses.contains(&new_address));

    Ok(())
}

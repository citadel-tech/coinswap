#![allow(unused)]
use std::{fs, path::PathBuf, process::Command, sync::{Arc, RwLock}, thread, time::Duration};
use std::str::FromStr;
use bitcoin::{Address, Amount, Network};
use bitcoind::{bitcoincore_rpc::RpcApi, tempfile, BitcoinD, Conf};
use std::sync::mpsc;
use std::io::BufRead;

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
        
        // Use cookie authentication instead of rpcuser/rpcpassword
        conf.args.push("-rpcport=18443");
        conf.args.push("-server=1");

        let bitcoind = BitcoinD::with_conf(bitcoind::exe_path()?, &conf)?;

        let mining_address = bitcoind.client.get_new_address(None, None)?
            .require_network(Network::Regtest)?;
        bitcoind.client.generate_to_address(101, &mining_address)?;

        let data_dir = temp_dir.join("maker");
        fs::create_dir_all(&data_dir)?;
        // Get the cookie file path from bitcoind's data directory
        // let cookie_file = bitcoind.conf.datadir.join(".cookie");
        let cookie_file = bitcoind.params.cookie_file.clone();
        let cookie_contents = fs::read_to_string(&cookie_file)?;
        let auth: Vec<&str> = cookie_contents.split(':').collect();

        // Create maker config file with cookie auth
        let config_contents = format!(r#"
            [maker_config]
            port = 6102
            rpc_port = 18443
            network = "clearnet"
            rpc_url = "127.0.0.1:18443"
            rpc_user = "{}"
            rpc_pass = "{}"
            rpc_network = "regtest"
            wallet_name = "maker"
            data_directory = "{}"
        "#, auth[0], auth[1], data_dir.display());
        
        let config_path = data_dir.join("config.toml");
        fs::write(&config_path, config_contents)?;

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
            let mut child = Command::new("cargo")
                .args(&[
                    "run",
                    "--bin",
                    "makerd",
                    "--",
                    "--data-directory", data_dir.to_str().unwrap(),
                    "--network", "clearnet"
                ])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("Failed to execute makerd");

            let stdout = child.stdout.take().unwrap();
            let stderr = child.stderr.take().unwrap();
            
            let stdout_reader = std::io::BufReader::new(stdout);
            let stderr_reader = std::io::BufReader::new(stderr);

            for line in stdout_reader.lines() {
                if let Ok(line) = line {
                    println!("stdout: {}", line);
                    tx.send(line).unwrap();
                }
            }

            for line in stderr_reader.lines() {
                if let Ok(line) = line {
                    println!("stderr: {}", line);
                    tx.send(line).unwrap();
                }
            }
        });

        Ok(rx)
    }

    fn wait_for_maker_setup(&self, rx: mpsc::Receiver<String>) -> Result<String, Box<dyn std::error::Error>> {
        let timeout = Duration::from_secs(30);
        let start = std::time::Instant::now();
        
        while start.elapsed() < timeout {
            match rx.try_recv() {
                Ok(line) => {
                    println!("makerd output: {}", line);
                    if line.contains("Fidelity bond address:") {
                        return Ok(line.split(":").last().unwrap().trim().to_string());
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {
                    thread::sleep(Duration::from_millis(100));
                    continue;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err("makerd process terminated unexpectedly".into());
                }
            }
        }
        Err("Timeout waiting for fidelity bond address".into())
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

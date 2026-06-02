//! Basic Taker API Example (Electrum backend)
//! ## No Prerequisites
//! ## Usage
//!
//! ```bash
//! # When prompted for encryption passphrase, press Enter for no encryption
//! cargo run --example electrum_taker_basic
//! ```

#[cfg(feature = "integration-test")]
fn main() {}
#[cfg(not(feature = "integration-test"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use bitcoin::Amount;
    use bitcoind::{bitcoincore_rpc::RpcApi, BitcoinD};
    use coinswap::{
        protocol::common_messages::ProtocolVersion,
        taker::{SwapParams, Taker, TakerInitConfig},
        wallet::{AddressType, BackendConfig, ElectrumBackend, ElectrumConfig},
    };
    use electrsd::ElectrsD;

    println!("=== Coinswap Taker Basic Example (Electrum backend) ===");
    println!("NOTE: When prompted for encryption passphrase, press Enter for no encryption");

    let wallet_path = std::env::home_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(".coinswap")
        .join("taker")
        .join("wallets")
        .join("electrum-taker-example");

    if wallet_path.exists() {
        std::fs::remove_file(&wallet_path).ok();
        println!("Cleaned up previous wallet file");
    }

    let data_dir = std::env::temp_dir().join("coinswap_electrum_taker_example");
    std::fs::create_dir_all(&data_dir)?;

    println!("Starting Bitcoin Core in regtest mode...");
    let mut conf = bitcoind::Conf::default();
    conf.args.push("-txindex=1");
    conf.args.push("-rest=1");
    conf.args.push("-deprecatedrpc=warnings");
    conf.p2p = bitcoind::P2P::Yes;
    conf.staticdir = Some(data_dir.join(".bitcoin"));
    let bitcoind = BitcoinD::with_conf(bitcoind::exe_path().unwrap(), &conf).unwrap();

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
    println!("Bitcoin Core started and initial blocks generated");

    println!("Starting electrs...");
    let mut electrs_conf = electrsd::Conf::default();
    let electrs_dir = data_dir.join("electrs");
    std::fs::create_dir_all(&electrs_dir)?;
    electrs_conf.staticdir = Some(electrs_dir);
    let electrsd = ElectrsD::with_conf(electrsd::exe_path().unwrap(), &bitcoind, &electrs_conf)
        .expect("failed to spawn electrs");
    let electrum_url = format!("tcp://{}", electrsd.electrum_url);
    println!("electrs available at {electrum_url}");

    let electrum_cfg = ElectrumConfig {
        url: electrum_url,
        wallet_name: "electrum-taker-example".to_string(),
    };

    println!("About to initialize taker...");
    let config = TakerInitConfig::default().with_backend(BackendConfig::Electrum(electrum_cfg));
    let taker = Taker::<ElectrumBackend>::init(config).unwrap();
    println!("Taker initialized successfully!");

    {
        let wallet = taker.get_wallet().read().unwrap();
        let balances = wallet.get_balances().unwrap();
        let utxos = wallet.list_all_utxo();
        println!("Initial wallet state:");
        println!("  Spendable: {} BTC", balances.spendable.to_btc());
        println!("  Regular: {} BTC", balances.regular.to_btc());
        println!("  UTXOs: {}", utxos.len());
    }

    {
        let balances = taker.get_wallet().read().unwrap().get_balances().unwrap();
        if balances.spendable == Amount::ZERO {
            println!("\nFunding wallet with test coins...");
            let mut wallet = taker.get_wallet().write().unwrap();
            let funding_address = wallet.get_next_external_address(AddressType::P2TR).unwrap();

            let fund_amount = Amount::from_btc(0.01).unwrap();
            bitcoind
                .client
                .send_to_address(
                    &funding_address,
                    fund_amount,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
                .unwrap();
            bitcoind
                .client
                .generate_to_address(1, &mining_address)
                .unwrap();
            let _ = electrsd.trigger();
            std::thread::sleep(std::time::Duration::from_secs(1));

            wallet.sync_and_save().unwrap();
            let updated_balances = wallet.get_balances().unwrap();
            println!("Wallet funded successfully!");
            println!("  New balance: {} BTC", updated_balances.spendable.to_btc());
        }
    }

    {
        let mut wallet = taker.get_wallet().write().unwrap();
        let new_address = wallet.get_next_external_address(AddressType::P2TR).unwrap();
        println!("\nGenerated new receiving address: {}", new_address);

        let final_balances = wallet.get_balances().unwrap();
        println!("\nFinal wallet balances:");
        println!("  Spendable: {} BTC", final_balances.spendable.to_btc());
        println!("  Regular: {} BTC", final_balances.regular.to_btc());

        let utxos = wallet.list_all_utxo();
        println!("\nUTXO information:");
        println!("  Total UTXOs: {}", utxos.len());
        if !utxos.is_empty() {
            println!(
                "  Sample UTXO: {} ({} BTC)",
                utxos[0].txid,
                utxos[0].amount.to_btc()
            );
        }
    }

    println!("\nCoinswap setup:");
    let swap_params = SwapParams {
        protocol: ProtocolVersion::Legacy,
        send_amount: Amount::from_btc(0.005).unwrap(),
        maker_count: 2,
        ..Default::default()
    };
    println!("Swap parameters:");
    println!(
        "  Amount: {} BTC ({} sats)",
        swap_params.send_amount.to_btc(),
        swap_params.send_amount.to_sat()
    );
    println!("  Makers needed: {}", swap_params.maker_count);
    println!("  Protocol: {:?}", swap_params.protocol);

    println!("\nIn production, you would call:");
    println!("  let summary = taker.prepare_coinswap(swap_params).unwrap();");
    println!("  let report = taker.start_coinswap(&summary.swap_id).unwrap();");

    println!("\nExample completed.");
    let _ = bitcoind.client.stop();
    println!("Bitcoin Core stopped.");
    Ok(())
}

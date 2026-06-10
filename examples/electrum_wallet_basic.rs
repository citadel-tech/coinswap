//! Basic Wallet API Example (Electrum backend)
//!
//! ## Usage
//!
//! ```bash
//! cargo run --example electrum_wallet_basic
//! ```

use bitcoin::Amount;
use bitcoind::{bitcoincore_rpc::RpcApi, BitcoinD};
use coinswap::{
    utill::MIN_FEE_RATE,
    wallet::{AddressType, Destination, ElectrumBackend, ElectrumConfig, Wallet},
};
use electrsd::ElectrsD;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Coinswap Wallet Basic Example (Electrum backend) ===");

    let wallet_path = std::env::home_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(".coinswap")
        .join("taker")
        .join("wallets")
        .join("electrum-wallet-example");

    if wallet_path.exists() {
        std::fs::remove_file(&wallet_path).ok();
        println!("Cleaned up previous wallet file");
    }

    let data_dir = std::env::temp_dir().join("coinswap_electrum_wallet_example");
    std::fs::create_dir_all(&data_dir)?;

    // bitcoind: regtest funding source. P2P enabled so electrs can attach.
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

    // electrs attached to the regtest bitcoind.
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
        wallet_name: "electrum-wallet-example".to_string(),
    };

    std::fs::create_dir_all(wallet_path.parent().unwrap())?;
    println!("About to initialize wallet...");
    let mut wallet = Wallet::<ElectrumBackend>::init(&wallet_path, &electrum_cfg, None).unwrap();
    println!("Wallet initialized successfully!");

    wallet.sync_and_save().unwrap();
    println!("Wallet synced with Electrum server");

    // Fund the wallet via bitcoind.
    let funding_address = wallet.get_next_external_address(AddressType::P2TR).unwrap();
    let fund_amount = Amount::from_btc(0.05).unwrap();
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
    println!("Wallet funded with {} BTC", fund_amount.to_btc());

    println!("\nBalance Types:");
    let balances = wallet.get_balances().unwrap();
    println!("  Spendable: {} BTC", balances.spendable.to_btc());
    println!("  Regular: {} BTC", balances.regular.to_btc());
    println!("  Swap: {} BTC", balances.swap.to_btc());
    println!("  Fidelity: {} BTC", balances.fidelity.to_btc());
    println!("  Contract: {} BTC", balances.contract.to_btc());

    println!("\nUTXO Summary:");
    let utxos = wallet.list_all_utxo();
    println!("  Total UTXOs: {}", utxos.len());

    println!("\nAddress Generation:");
    let external_address = wallet.get_next_external_address(AddressType::P2TR).unwrap();
    let internal_addresses = wallet
        .get_next_internal_addresses(2, AddressType::P2TR)
        .unwrap();
    println!("  External (receiving): {external_address}");
    println!(
        "  Internal (change): {} {}",
        internal_addresses[0], internal_addresses[1]
    );

    if balances.spendable > Amount::ZERO {
        let select_amount = Amount::from_btc(0.001).unwrap();
        if balances.spendable >= select_amount {
            println!("\nCoin Selection Demo:");
            let selected_utxos = wallet
                .coin_select(select_amount, MIN_FEE_RATE, None, None)
                .unwrap();
            println!(
                "  Selected {} UTXOs for {} BTC",
                selected_utxos.len(),
                select_amount.to_btc()
            );

            let destination_address = internal_addresses[0].clone();
            let transaction = wallet
                .spend_coins(
                    &selected_utxos,
                    Destination::Sweep(destination_address.clone()),
                    MIN_FEE_RATE,
                )
                .unwrap();
            println!("  Created transaction: {}", transaction.compute_txid());
            println!("  Inputs: {}", transaction.input.len());
            println!("  Outputs: {}", transaction.output.len());
            println!("  Destination: {destination_address}");
        }
    }

    println!("\nExample completed.");

    let _ = bitcoind.client.stop();
    println!("Bitcoin Core stopped.");
    Ok(())
}

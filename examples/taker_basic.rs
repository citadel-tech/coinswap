//! Basic Taker API Example
//!
//! This example demonstrates how to use the Coinswap Taker API:
//! - Initialize a Taker instance with Bitcoin Core RPC
//! - Check wallet balance and generate addresses  
//! - Fund the wallet with test coins
//! - Discover available makers through offer discovery
//! - Set up coinswap parameters
//!
//! ## Usage
//!
//! ```bash
//! cargo run --example taker_basic --features integration-test
//! ```

use bitcoin::Amount;
use bitcoind::{
    bitcoincore_rpc::{Auth, RpcApi},
    BitcoinD,
};
use coinswap::{
    taker::{SwapParams, Taker, TakerBehavior},
    utill::ConnectionType,
    wallet::RPCConfig,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Coinswap Taker Basic Example ===");

    // Setup bitcoind in regtest mode
    let data_dir = std::env::temp_dir().join("coinswap_taker_example");
    std::fs::create_dir_all(&data_dir)?;

    println!("Starting Bitcoin Core in regtest mode...");

    // Setup bitcoind configuration
    let mut conf = bitcoind::Conf::default();
    conf.args.push("-txindex=1"); // Required for wallet sync
    conf.staticdir = Some(data_dir.join(".bitcoin"));

    let exe_path = bitcoind::exe_path().unwrap();
    let bitcoind = BitcoinD::with_conf(exe_path, &conf).unwrap();

    // Generate initial 101 blocks (required for coinbase maturity)
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

    // Create RPC config from bitcoind instance
    let rpc_config = RPCConfig {
        url: bitcoind.rpc_url().split_at(7).1.to_string(), // Remove "http://" prefix
        auth: Auth::CookieFile(bitcoind.params.cookie_file.clone()),
        wallet_name: "taker-example".to_string(), // Use specific wallet name
    };

    // Clean up any existing wallet files to avoid network mismatch
    let wallet_dir = std::env::temp_dir()
        .join("coinswap_taker_example")
        .join("taker-example");
    if wallet_dir.exists() {
        std::fs::remove_file(&wallet_dir).ok();
    }

    // Initialize Taker with default data directory and wallet name
    let mut taker = Taker::init(
        None,                              // Use default data directory
        Some("taker-example".to_string()), // Wallet file name
        Some(rpc_config),                  // Bitcoin Core RPC connection
        TakerBehavior::Normal,             // Normal behavior (TODO: make test-only)
        None,                              // Default port
        None,                              // Default connection string
        Some(ConnectionType::CLEARNET),    // Connection type (TODO: remove)
    )
    .unwrap();

    println!("Taker initialized successfully");

    // Check initial wallet balance and UTXOs
    let wallet = taker.get_wallet();
    let balances = wallet.get_balances().unwrap();
    let utxos = wallet.get_all_utxo().unwrap();

    println!("Initial wallet state:");
    println!("  Spendable: {} BTC", balances.spendable.to_btc());
    println!("  Regular: {} BTC", balances.regular.to_btc());
    println!("  UTXOs: {}", utxos.len());

    // Fund the wallet if empty
    if balances.spendable == Amount::ZERO {
        println!("\nFunding wallet with test coins...");

        let wallet_mut = taker.get_wallet_mut();
        let funding_address = wallet_mut.get_next_external_address().unwrap();

        // Send coins from bitcoind to the taker wallet
        let fund_amount = Amount::from_btc(0.01).unwrap();
        let _txid = bitcoind
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

        // Mine a block to confirm the transaction
        bitcoind
            .client
            .generate_to_address(1, &mining_address)
            .unwrap();

        // Sync wallet to see the new funds
        wallet_mut.sync_and_save().unwrap();

        let updated_balances = wallet_mut.get_balances().unwrap();
        println!("Wallet funded successfully!");
        println!("  New balance: {} BTC", updated_balances.spendable.to_btc());
    }

    // Generate a new receiving address
    let wallet_mut = taker.get_wallet_mut();
    let new_address = wallet_mut.get_next_external_address().unwrap();
    println!("\nGenerated new receiving address: {}", new_address);

    // Demonstrate sending coins (send small amount to ourselves)
    println!("\nDemonstrating send functionality:");
    let send_amount = Amount::from_btc(0.001).unwrap();
    let internal_address = wallet_mut.get_next_internal_addresses(1).unwrap()[0].clone();

    println!(
        "Sending {} BTC to internal address: {}",
        send_amount.to_btc(),
        internal_address
    );
    // Note: In production you would call: wallet.send_to_address(&address, amount, fee_rate)
    println!("(Send functionality ready - commented out to avoid spending coins in example)");

    // Show current balances
    let final_balances = wallet_mut.get_balances().unwrap();
    println!("\nFinal wallet balances:");
    println!("  Spendable: {} BTC", final_balances.spendable.to_btc());
    println!("  Regular: {} BTC", final_balances.regular.to_btc());
    println!("  Swap: {} BTC", final_balances.swap.to_btc());
    println!("  Fidelity: {} BTC", final_balances.fidelity.to_btc());
    println!("  Contract: {} BTC", final_balances.contract.to_btc());

    // Show UTXOs
    let utxos = wallet_mut.get_all_utxo().unwrap();
    println!("\nUTXO information:");
    println!("  Total UTXOs: {}", utxos.len());
    if !utxos.is_empty() {
        println!(
            "  Sample UTXO: {} ({} BTC)",
            utxos[0].txid,
            utxos[0].amount.to_btc()
        );
    }

    // Coinswap setup
    println!("\nCoinswap setup:");

    let swap_params = SwapParams {
        send_amount: Amount::from_btc(0.005).unwrap(),
        maker_count: 2,
        tx_count: 3, // TODO: remove this field
    };

    println!("Swap parameters:");
    println!(
        "  Amount: {} BTC ({} sats)",
        swap_params.send_amount.to_btc(),
        swap_params.send_amount.to_sat()
    );
    println!("  Makers needed: {}", swap_params.maker_count);
    println!("  Transactions per maker: {}", swap_params.tx_count);

    // TODO: Uncomment this when we want to test the actual error
    // For now, commenting out since it hangs for too long
    /*
    println!("Attempting coinswap...");
    let result = taker.do_coinswap(swap_params).unwrap();
    println!("Coinswap completed successfully: {:?}", result);
    */

    println!("Coinswap call commented out to avoid long timeout.");
    println!("\nIn production, you would call:");
    println!("  let result = taker.do_coinswap(swap_params).unwrap();");
    println!("\nThis would:");
    println!("- Connect to the directory server to find makers");
    println!("- Negotiate with {} makers", swap_params.maker_count);
    println!("- Execute the coinswap protocol");
    println!("- Return the final transaction details");

    println!("\nExample completed. Bitcoin Core will shutdown automatically.");

    // bitcoind will be automatically stopped when it goes out of scope
    Ok(())
}

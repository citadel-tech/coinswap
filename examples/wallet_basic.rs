//! Basic Wallet API Example
//!
//! This example demonstrates comprehensive wallet operations through the Taker API:
//! - Check all balance types (regular, swap, fidelity, contract)
//! - Generate external and internal addresses
//! - UTXO categorization and detailed spend info
//! - Coin selection algorithms
//! - Transaction creation and broadcasting
//! - UTXO management and locking
//! - Contract monitoring (timelock, hashlock)
//! - Swap coin operations
//!
//! ## Usage
//!
//! ```bash
//! cargo run --example wallet_basic --features integration-test
//! ```

use bitcoin::Amount;
use bitcoind::{
    bitcoincore_rpc::{Auth, RpcApi},
    BitcoinD,
};
#[cfg(feature = "integration-test")]
use coinswap::taker::TakerBehavior;
use coinswap::{
    taker::Taker,
    utill::MIN_FEE_RATE,
    wallet::{Destination, RPCConfig},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Coinswap Wallet Basic Example ===");

    // Setup bitcoind in regtest mode
    let data_dir = std::env::temp_dir().join("coinswap_wallet_example");
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
        wallet_name: "wallet-example".to_string(), // Use specific wallet name
    };

    // Initialize Taker to access wallet (Wallet API is not publicly exposed)
    let mut taker = Taker::init(
        None,                               // Use default data directory
        Some("wallet-example".to_string()), // Wallet file name
        Some(rpc_config),                   // Bitcoin Core RPC connection
        #[cfg(feature = "integration-test")]
        TakerBehavior::Normal, // behavior
        None,                               // Default port
        None,                               // Default connection string
    )
    .unwrap();

    println!("Wallet initialized successfully (via Taker API)");

    // Sync wallet first
    let wallet_mut = taker.get_wallet_mut();
    wallet_mut.sync_and_save().unwrap();
    println!("Wallet synced with blockchain");

    // Fund the wallet for demonstration
    let funding_address = wallet_mut.get_next_external_address().unwrap();
    let fund_amount = Amount::from_btc(0.05).unwrap();
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
    println!("Wallet funded with {} BTC", fund_amount.to_btc());

    // Get all wallet data (in scope to avoid borrow conflicts)
    let (
        balances,
        utxos,
        swapcoins_count,
        regular_utxos,
        swap_utxos,
        fidelity_utxos,
        swept_utxos,
        external_index,
    ) = {
        let wallet = taker.get_wallet();
        (
            wallet.get_balances().unwrap(),
            wallet.get_all_utxo_from_rpc().unwrap(),
            wallet.get_swapcoins_count(),
            wallet.list_descriptor_utxo_spend_info().unwrap(),
            wallet.list_swap_coin_utxo_spend_info().unwrap(),
            wallet.list_fidelity_spend_info().unwrap(),
            wallet.list_swept_incoming_swap_utxos().unwrap(),
            *wallet.get_external_index(),
        )
    };

    // Check balances and explain what each type means
    println!("\nBalance Types:");
    println!(
        "  Spendable: {} BTC (regular + swap coins available for spending)",
        balances.spendable.to_btc()
    );
    println!(
        "  Regular: {} BTC (normal wallet UTXOs)",
        balances.regular.to_btc()
    );
    println!(
        "  Swap: {} BTC (completed swap coins)",
        balances.swap.to_btc()
    );
    println!(
        "  Fidelity: {} BTC (locked in fidelity bonds)",
        balances.fidelity.to_btc()
    );
    println!(
        "  Contract: {} BTC (locked in active contracts)",
        balances.contract.to_btc()
    );

    // Show total UTXOs and swap coin count
    println!("\nUTXO Summary:");
    println!("  Total UTXOs: {}", utxos.len());
    println!("  Swap coins: {}", swapcoins_count);

    // Categorize UTXOs by type
    println!("\nUTXO Categories:");
    println!("  Regular UTXOs: {}", regular_utxos.len());
    println!("  Swap UTXOs: {}", swap_utxos.len());
    println!("  Fidelity UTXOs: {}", fidelity_utxos.len());
    println!("  Swept swap UTXOs: {}", swept_utxos.len());

    // Generate addresses
    let (external_address, internal_addresses) = {
        let wallet_mut = taker.get_wallet_mut();
        (
            wallet_mut.get_next_external_address().unwrap(),
            wallet_mut.get_next_internal_addresses(2).unwrap(),
        )
    };

    println!("\nAddress Generation:");
    println!("  External (receiving): {}", external_address);
    println!(
        "  Internal (change): {} {}",
        internal_addresses[0], internal_addresses[1]
    );

    // Show wallet state information
    println!("\nWallet State:");
    println!("  External address index: {}", external_index);

    // Demonstrate UTXO management
    println!("\nUTXO Management:");
    {
        let wallet = taker.get_wallet();
        wallet.lock_unspendable_utxos().unwrap();
    }
    println!("  Locked unspendable UTXOs (fidelity bonds, contracts)");

    // Demonstrate coin selection and transaction operations
    if balances.spendable > Amount::ZERO {
        let select_amount = Amount::from_btc(0.001).unwrap();
        if balances.spendable >= select_amount {
            println!("\nCoin Selection Demo:");
            let selected_utxos = {
                let wallet = taker.get_wallet();
                wallet
                    .coin_select(select_amount, MIN_FEE_RATE, None)
                    .unwrap()
            };

            let total_selected: u64 = selected_utxos
                .iter()
                .map(|(utxo, _)| utxo.amount.to_sat())
                .sum();
            println!(
                "  Selected {} UTXOs for {} BTC",
                selected_utxos.len(),
                select_amount.to_btc()
            );
            println!(
                "  Total value: {} BTC",
                Amount::from_sat(total_selected).to_btc()
            );

            // Show types of selected UTXOs
            for (utxo, spend_info) in selected_utxos.iter().take(3) {
                println!(
                    "    UTXO: {} ({} BTC, type: {})",
                    utxo.txid,
                    utxo.amount.to_btc(),
                    spend_info
                );
            }

            // Demonstrate transaction creation
            println!("\nTransaction Creation Demo:");
            let destination_address = internal_addresses[0].clone();

            let transaction = {
                let wallet_mut = taker.get_wallet_mut();
                wallet_mut
                    .spend_coins(
                        &selected_utxos,
                        Destination::Sweep(destination_address.clone()),
                        MIN_FEE_RATE,
                    )
                    .unwrap()
            };

            let txid = transaction.compute_txid();
            println!("  Created transaction: {}", txid);
            println!("  Inputs: {}", transaction.input.len());
            println!("  Outputs: {}", transaction.output.len());
            println!("  Size: {} bytes", transaction.vsize());
            println!("  Destination: {}", destination_address);

            // This is how this will be broadcasted in production
            println!("  Transaction ready for broadcast with wallet.send_tx()");
            // let broadcast_txid = wallet.send_tx(&transaction).unwrap();
        } else {
            println!("\nCoin Selection Demo:");
            println!(
                "  Insufficient funds for coin selection demo (need {} BTC)",
                select_amount.to_btc()
            );
        }

        // Demonstrate swap operations
        println!("\nSwap Operations:");
        if !swap_utxos.is_empty() {
            println!("  Found {} incoming swap coin UTXOs", swap_utxos.len());
            println!("  Use wallet.sweep_incoming_swapcoins() to sweep completed swaps");
        } else {
            println!("  No incoming swap coins to sweep");
        }
    }

    // Show advanced wallet operations
    println!("\nAdvanced Wallet Information:");

    // Get advanced wallet info
    let (live_contracts, timelock_contracts, hashlock_contracts, all_utxo_info) = {
        let wallet = taker.get_wallet();
        (
            wallet.list_live_contract_spend_info().unwrap(),
            wallet.list_live_timelock_contract_spend_info().unwrap(),
            wallet.list_live_hashlock_contract_spend_info().unwrap(),
            wallet.list_all_utxo_spend_info().unwrap(),
        )
    };

    println!("  Live contracts: {}", live_contracts.len());
    println!("  Timelock contracts: {}", timelock_contracts.len());
    println!("  Hashlock contracts: {}", hashlock_contracts.len());
    println!(
        "  Detailed UTXO info available for {} UTXOs",
        all_utxo_info.len()
    );

    // Show first few UTXOs with their spend info
    for (utxo, spend_info) in all_utxo_info.iter().take(3) {
        println!(
            "    {} {} BTC ({})",
            utxo.txid,
            utxo.amount.to_btc(),
            spend_info
        );
    }

    println!("\nWallet is funded and ready for operations!");
    println!("Example completed. Bitcoin Core will shutdown automatically.");

    // bitcoind will be automatically stopped when it goes out of scope
    Ok(())
}

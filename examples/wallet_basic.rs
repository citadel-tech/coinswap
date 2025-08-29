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
//! ## Prerequisites
//!
//! Start Bitcoin Core in regtest mode:
//! ```bash
//! bitcoind -regtest -server -rpcuser=user -rpcpassword=password -rpcport=18443
//! ```
//!
//! ## Usage
//!
//! ```bash
//! cargo run --example wallet_basic --features integration-test
//! ```

use bitcoin::Amount;
use bitcoind::bitcoincore_rpc::Auth;
use coinswap::{
    taker::{Taker, TakerBehavior},
    utill::{ConnectionType, MIN_FEE_RATE},
    wallet::{Destination, RPCConfig},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Coinswap Wallet Basic Example");

    // Setup
    let data_dir = std::env::temp_dir()
        .join("coinswap_examples")
        .join("wallet_basic");
    std::fs::create_dir_all(&data_dir)?;

    let rpc_config = RPCConfig {
        url: "127.0.0.1:18443".to_string(),
        auth: Auth::UserPass("user".to_string(), "password".to_string()),
        wallet_name: "".to_string(),
    };

    // Initialize Taker to access wallet
    let mut taker = Taker::init(
        Some(data_dir),
        None,
        Some(rpc_config),
        TakerBehavior::Normal,
        None,
        None,
        Some(ConnectionType::CLEARNET),
    )
    .map_err(|e| format!("Failed to initialize Taker: {:?}", e))?;

    // Sync wallet first
    let wallet_mut = taker.get_wallet_mut();
    wallet_mut
        .sync_and_save()
        .map_err(|e| format!("Failed to sync wallet: {:?}", e))?;
    println!("Wallet synced with blockchain");

    // Get all wallet data
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
        let balances = wallet
            .get_balances()
            .map_err(|e| format!("Failed to get balances: {:?}", e))?;
        let utxos = wallet
            .get_all_utxo()
            .map_err(|e| format!("Failed to get UTXOs: {:?}", e))?;
        let swapcoins_count = wallet.get_swapcoins_count();
        let regular_utxos = wallet
            .list_descriptor_utxo_spend_info()
            .map_err(|e| format!("Failed to get regular UTXOs: {:?}", e))?;
        let swap_utxos = wallet
            .list_swap_coin_utxo_spend_info()
            .map_err(|e| format!("Failed to get swap UTXOs: {:?}", e))?;
        let fidelity_utxos = wallet
            .list_fidelity_spend_info()
            .map_err(|e| format!("Failed to get fidelity UTXOs: {:?}", e))?;
        let swept_utxos = wallet
            .list_swept_incoming_swap_utxos()
            .map_err(|e| format!("Failed to get swept UTXOs: {:?}", e))?;
        let external_index = *wallet.get_external_index();
        (
            balances,
            utxos,
            swapcoins_count,
            regular_utxos,
            swap_utxos,
            fidelity_utxos,
            swept_utxos,
            external_index,
        )
    };

    // Check balances and explain what each type means
    println!("Balance Types:");
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
    println!("UTXO Summary:");
    println!("  Total UTXOs: {}", utxos.len());
    println!("  Swap coins: {}", swapcoins_count);

    // Categorize UTXOs by type
    println!("UTXO Categories:");
    println!("  Regular UTXOs: {}", regular_utxos.len());
    println!("  Swap UTXOs: {}", swap_utxos.len());
    println!("  Fidelity UTXOs: {}", fidelity_utxos.len());
    println!("  Swept swap UTXOs: {}", swept_utxos.len());

    // Generate addresses
    let (external_address, internal_addresses) = {
        let wallet_mut = taker.get_wallet_mut();
        let external_address = wallet_mut
            .get_next_external_address()
            .map_err(|e| format!("Failed to generate external address: {:?}", e))?;
        let internal_addresses = wallet_mut
            .get_next_internal_addresses(2)
            .map_err(|e| format!("Failed to generate internal addresses: {:?}", e))?;
        (external_address, internal_addresses)
    };

    println!("Address Generation:");
    println!("  External (receiving): {}", external_address);
    println!(
        "  Internal (change): {} {}",
        internal_addresses[0], internal_addresses[1]
    );

    // Show wallet state information
    println!("Wallet State:");
    println!("  External address index: {}", external_index);

    // Demonstrate UTXO management
    println!("UTXO Management:");
    {
        let wallet = taker.get_wallet();
        match wallet.lock_unspendable_utxos() {
            Ok(()) => println!("  Locked unspendable UTXOs (fidelity bonds, contracts)"),
            Err(e) => println!("  Failed to lock unspendable UTXOs: {:?}", e),
        }
    }

    // Demonstrate coin selection and transaction operations if wallet has funds
    if balances.spendable > Amount::ZERO {
        let select_amount = Amount::from_btc(0.001).unwrap();
        if balances.spendable >= select_amount {
            println!("Coin Selection Demo:");
            let selected_utxos = {
                let wallet = taker.get_wallet();
                wallet.coin_select(select_amount, MIN_FEE_RATE)
            };

            match selected_utxos {
                Ok(selected_utxos) => {
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
                    println!("Transaction Creation Demo:");
                    let destination_address = internal_addresses[0].clone();

                    let transaction = {
                        let wallet_mut = taker.get_wallet_mut();
                        wallet_mut.spend_coins(
                            &selected_utxos,
                            Destination::Sweep(destination_address.clone()),
                            MIN_FEE_RATE,
                        )
                    };

                    match transaction {
                        Ok(transaction) => {
                            let txid = transaction.compute_txid();
                            println!("  Created transaction: {}", txid);
                            println!("  Inputs: {}", transaction.input.len());
                            println!("  Outputs: {}", transaction.output.len());
                            println!("  Size: {} bytes", transaction.vsize());
                            println!("  Destination: {}", destination_address);

                            // This is how this will be broadcasted in production
                            println!("  Transaction ready for broadcast with wallet.send_tx()");
                            // let broadcast_txid = wallet.send_tx(&transaction)?;
                        }
                        Err(e) => println!("  Transaction creation failed: {:?}", e),
                    }
                }
                Err(e) => println!("  Coin selection failed: {:?}", e),
            }
        } else {
            println!("Coin Selection Demo:");
            println!(
                "  Insufficient funds for coin selection demo (need {} BTC)",
                select_amount.to_btc()
            );
        }

        // Demonstrate swap operations
        println!("Swap Operations:");
        if !swap_utxos.is_empty() {
            println!("  Found {} incoming swap coin UTXOs", swap_utxos.len());
            println!("  Use wallet.sweep_incoming_swapcoins() to sweep completed swaps");
        } else {
            println!("  No incoming swap coins to sweep");
        }
    }

    // Show advanced wallet operations
    println!("Advanced Wallet Information:");

    // Get advanced wallet info in a separate scope
    let (live_contracts, timelock_contracts, hashlock_contracts, all_utxo_info) = {
        let wallet = taker.get_wallet();
        let live_contracts = wallet
            .list_live_contract_spend_info()
            .map_err(|e| format!("Failed to get live contracts: {:?}", e))?;
        let timelock_contracts = wallet
            .list_live_timelock_contract_spend_info()
            .map_err(|e| format!("Failed to get timelock contracts: {:?}", e))?;
        let hashlock_contracts = wallet
            .list_live_hashlock_contract_spend_info()
            .map_err(|e| format!("Failed to get hashlock contracts: {:?}", e))?;
        let all_utxo_info = wallet
            .list_all_utxo_spend_info()
            .map_err(|e| format!("Failed to get detailed UTXO info: {:?}", e))?;
        (
            live_contracts,
            timelock_contracts,
            hashlock_contracts,
            all_utxo_info,
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

    if balances.spendable == Amount::ZERO {
        println!("\nTo fund wallet:");
        println!("  bitcoin-cli -regtest createwallet \"test\"");
        println!("  bitcoin-cli -regtest generatetoaddress 101 $(bitcoin-cli -regtest -rpcwallet=test getnewaddress)");
        println!("  bitcoin-cli -regtest -rpcwallet=test settxfee 0.00001");
        println!(
            "  bitcoin-cli -regtest -rpcwallet=test sendtoaddress {} 0.01",
            external_address
        );
        println!("  bitcoin-cli -regtest generatetoaddress 1 $(bitcoin-cli -regtest -rpcwallet=test getnewaddress)");
    } else {
        println!("\nWallet is funded and ready for operations!");
    }

    Ok(())
}

//! Basic Taker API Example
//!
//! This example demonstrates how to use the Coinswap Taker API:
//! - Initialize a Taker instance with Bitcoin Core RPC
//! - Check wallet balance and generate addresses  
//! - Sync with directory server and fetch maker offers
//! - Find suitable makers and display offer details
//! - Set up coinswap parameters
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
//! cd examples/taker_basic
//! cargo run --features integration-test
//! ```

use bitcoin::Amount;
use bitcoind::bitcoincore_rpc::Auth;
use coinswap::{
    taker::{SwapParams, Taker, TakerBehavior},
    utill::ConnectionType,
    wallet::RPCConfig,
};
use log::{error, info};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Coinswap Taker Basic Example");

    // Setup data directory
    let data_dir = std::env::temp_dir()
        .join("coinswap_examples")
        .join("taker_basic");
    std::fs::create_dir_all(&data_dir)?;

    let rpc_config = RPCConfig {
        url: "127.0.0.1:18443".to_string(),
        auth: Auth::UserPass("user".to_string(), "password".to_string()),
        wallet_name: "".to_string(),
    };

    // Initialize Taker
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

    println!("Taker initialized successfully");

    // Check wallet balance and UTXOs
    let wallet = taker.get_wallet();
    let balances = wallet
        .get_balances()
        .map_err(|e| format!("Failed to get balances: {:?}", e))?;
    let utxos = wallet
        .get_all_utxo()
        .map_err(|e| format!("Failed to get UTXOs: {:?}", e))?;

    println!("Spendable: {} BTC", balances.spendable.to_btc());
    println!("UTXOs: {}", utxos.len());

    // Handle empty wallet
    if balances.spendable == Amount::ZERO {
        println!("Wallet is empty. Generate funding address:");
        let wallet_mut = taker.get_wallet_mut();
        let address = wallet_mut
            .get_next_external_address()
            .map_err(|e| format!("Failed to generate address: {:?}", e))?;
        println!("Address: {}", address);
        println!(
            "Fund with: bitcoin-cli -regtest -rpcwallet=test sendtoaddress {} 0.01",
            address
        );
        return Ok(());
    }

    // Wallet has funds - demonstrate offer fetching and maker discovery
    println!("\nMaker Discovery:");
    info!("Starting maker discovery");

    // Sync with directory server to get latest offers
    println!("  Syncing offerbook with directory server...");
    info!("Starting offerbook sync with directory server");
    match taker.sync_offerbook() {
        Ok(()) => {
            println!("  Offerbook sync completed");
            info!("Offerbook sync successful");

            // Fetch offers without trying to display individual offers
            match taker.fetch_offers() {
                Ok(offerbook) => {
                    let all_offers = offerbook.all_good_makers();
                    println!("  Found {} total offers", all_offers.len());
                    info!("Fetched {} offers", all_offers.len());

                    if all_offers.is_empty() {
                        println!("  No makers available in network");
                        println!(
                            "  In production, makers would be discovered via directory servers"
                        );
                        println!("  For testing, start maker instances to see real offers");
                        info!("No makers available");
                    } else {
                        println!("  Sample makers found:");
                        for (i, offer_addr) in all_offers.iter().take(3).enumerate() {
                            println!("    {}. {}", i + 1, offer_addr.address);
                        }
                        info!("Listed {} sample makers", all_offers.len().min(3));
                    }
                }
                Err(e) => {
                    error!("Failed to fetch offers: {:?}", e);
                    println!("  Failed to fetch offers: {:?}", e);
                }
            }
        }
        Err(e) => {
            println!("  Failed to sync offerbook: {:?}", e);
            println!("  This is normal in regtest without directory server running");
        }
    }

    // Configure swap parameters
    println!("\nCoinswap Setup:");

    let min_amount = Amount::from_btc(0.001)?;
    if balances.spendable < min_amount {
        println!("  Insufficient funds for coinswap demo");
        println!(
            "  Current: {} BTC, Minimum: {} BTC",
            balances.spendable.to_btc(),
            min_amount.to_btc()
        );
        return Ok(());
    }

    let swap_params = SwapParams {
        send_amount: Amount::from_btc(0.0005)?,
        maker_count: 2,
        tx_count: 3,
    };

    println!(
        "  Amount: {} BTC ({} sats)",
        swap_params.send_amount.to_btc(),
        swap_params.send_amount.to_sat()
    );
    println!("  Makers needed: {}", swap_params.maker_count);
    println!("  Transactions per maker: {}", swap_params.tx_count);

    let suitable_makers = taker.find_suitable_makers();
    println!("  Suitable makers found: {}", suitable_makers.len());

    if suitable_makers.len() >= swap_params.maker_count {
        println!("  Sufficient makers available for coinswap");
        println!("  Selected makers:");
        for (i, maker) in suitable_makers
            .iter()
            .take(swap_params.maker_count)
            .enumerate()
        {
            println!("    {}. {}", i + 1, maker.address);
        }
        println!("  Ready for coinswap execution (taker.do_coinswap())");
    } else {
        println!("  Not enough suitable makers");
        println!(
            "  Need: {}, Available: {}",
            swap_params.maker_count,
            suitable_makers.len()
        );
    }

    let bad_makers = taker.get_bad_makers();
    if !bad_makers.is_empty() {
        println!("\nBad Makers (will be avoided):");
        for bad_maker in bad_makers {
            println!("  - {} (failed previous interactions)", bad_maker.address);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_swap_params() {
        let swap_params = SwapParams {
            send_amount: Amount::from_btc(0.01).unwrap(),
            maker_count: 2,
            tx_count: 3,
        };

        assert_eq!(swap_params.send_amount, Amount::from_sat(1_000_000));
        assert_eq!(swap_params.maker_count, 2);
        assert_eq!(swap_params.tx_count, 3);
    }
}

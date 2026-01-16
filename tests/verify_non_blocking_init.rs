#![cfg(feature = "integration-test")]
use bitcoin::Amount;
use coinswap::{
    taker::{Taker, TakerBehavior},
    wallet::{AddressType, RPCConfig},
};
use std::sync::Arc;

mod test_framework;
use test_framework::*;

use log::{info, warn};

#[test]
fn test_non_blocking_wallet_init() {
    // ---- Setup ----
    // Initialize framework with no makers/takers to just get bitcoind
    let (test_framework, _, _, block_generation_handle) = TestFramework::init(vec![], vec![]);

    warn!("🧪 Running Test: Verify Non-Blocking Wallet Init");
    let bitcoind = &test_framework.bitcoind;
    let temp_dir = &test_framework.temp_dir;
    let zmq_addr = test_framework.zmq_addr.clone();

    // Init Taker manually (without auto-sync)
    let taker_id = "manual_taker".to_string();
    let rpc_config = RPCConfig::from(test_framework.as_ref());
    let mut taker = Taker::init(
        Some(temp_dir.join(&taker_id)),
        Some(taker_id),
        Some(rpc_config),
        TakerBehavior::Normal,
        None,
        None,
        zmq_addr,
        None,
    )
    .unwrap();

    // At this point, Taker is initialized but wallet should NOT be synced.
    // However, since it's a new wallet, it has no history.
    // To verify non-syncing behavior properly, we need to:
    // 1. Send coins to the taker's address.
    // 2. Generate blocks.
    // 3. Check balance (should be 0 because it hasn't scanned/synced).
    // 4. Connect/Sync wallet.
    // 5. Check balance (should be > 0).

    let wallet = taker.get_wallet_mut();
    // We can call sync_and_save here to init the wallet file properly if needed, BUT Taker::init already loads/creates it.
    // But Taker::init skipped the sync.
    // If we get an address now, it should work (derives from seed).
    let taker_address = wallet
        .get_next_external_address(AddressType::P2WPKH)
        .unwrap();

    // Send coins
    let utxo_value = Amount::from_btc(0.1).unwrap();
    send_to_address(bitcoind, &taker_address, utxo_value);
    generate_blocks(bitcoind, 6); // Generate confirmations

    // Check balance BEFORE sync.
    // Since we skipped sync in init, and haven't called it since,
    // the wallet should not know about these coins yet.
    // Note: Watcher runs in background, but it listens to ZMQ for *new* transactions.
    // However, we just sent a tx and mined blocks. Watcher might pick it up?
    // Watcher mainly handles swap protocol messages and certain txs.
    // Does it update wallet balance?
    // `Watcher` updates `FileRegistry`.
    // Use `wallet.get_balances()` which queries core RPC for UTXOs *known to the wallet*.
    // If wallet hasn't scanned/imported descriptors, it might not know them?
    // Actually, `wallet.get_balances` calls `list_descriptor_utxo_spend_info` which iterates over descriptors.
    // `list_descriptor_utxo_spend_info` uses `list_unspent` from RPC.
    // BUT `list_unspent` only returns UTXOs for addresses *imported* into the wallet.
    // When are addresses imported?
    // `Wallet::sync_and_save` calls `import_descriptors`.
    // If we skipped `sync_and_save`, `import_descriptors` was likely NOT called, or at least not updated.
    // `Wallet::load_or_init_wallet` -> `Wallet::init` or `Wallet::load`.
    // Neither `init` nor `load` calls `import_descriptors`.
    // `import_descriptors` is called inside `sync()`.
    // So if `sync()` was skipped, the descriptors are NOT imported in Bitcoin Core wallet.
    // Therefore `list_unspent` returns empty.

    // Verify balance is ZERO
    let balances_pre_sync = wallet.get_balances().unwrap();
    info!(
        "Helper check: Pre-sync regular balance: {}",
        balances_pre_sync.regular
    );
    // If imports didn't happen, balance should be 0.
    assert_eq!(
        balances_pre_sync.regular,
        Amount::ZERO,
        "Balance should be 0 before sync because descriptors are not imported yet"
    );

    // Drop mutable borrow of wallet to allow borrowing taker for sync
    // The borrow checker considers 'wallet' alive until its last use.
    // Since we used it above, we can just allow it to end scope or shadow it?
    // Actually, simply not using the 'wallet' variable from L50 anymore matches scope end if we re-bind.
    // But 'wallet' variable is holding the borrow.
    // We must ensure the compiler knows we are done with it.

    // Now Sync Manual
    info!("🔄 Manually syncing wallet...");
    taker.sync_wallet().unwrap();

    // Re-borrow wallet to check balance
    let wallet = taker.get_wallet_mut();
    // Verify balance is updated
    let balances_post_sync = wallet.get_balances().unwrap();
    info!(
        "Helper check: Post-sync regular balance: {}",
        balances_post_sync.regular
    );
    assert_eq!(
        balances_post_sync.regular, utxo_value,
        "Balance should be 0.1 BTC after sync"
    );

    test_framework.stop();
    block_generation_handle.join().unwrap();
}

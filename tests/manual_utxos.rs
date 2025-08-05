#![cfg(feature = "integration-test")]
use bitcoin::{Amount, OutPoint};
use coinswap::utill::MIN_FEE_RATE;
mod test_framework;
use test_framework::*;

/// This test checks that manual_utxo_outpoints are honored by coin_select.
#[test]
fn test_manual_utxo_selection() {
    // Setup: create a wallet and fund it with several UTXOs
    let (test_framework, _, makers, _directory_server_instance, _block_generation_handle) =
        TestFramework::init(
            [((6102, None), coinswap::maker::MakerBehavior::Normal)].into(),
            vec![coinswap::taker::TakerBehavior::Normal],
            coinswap::utill::ConnectionType::CLEARNET,
        );

    let bitcoind = &test_framework.bitcoind;
    let maker = makers.first().unwrap();

    // Fund 3 addresses with 0.1, 0.2, 0.3 BTC each
    for amount in [0.1, 0.2, 0.3] {
        let addr = maker
            .get_wallet()
            .write()
            .unwrap()
            .get_next_external_address()
            .unwrap();
        send_to_address(bitcoind, &addr, Amount::from_btc(amount).unwrap());
        generate_blocks(bitcoind, 1);
    }

    let utxo_outpoints = maker
        .get_wallet()
        .write()
        .unwrap()
        .get_all_utxo()
        .unwrap()
        .iter()
        .map(|utxo| OutPoint::new(utxo.txid, utxo.vout))
        .collect::<Vec<_>>();

    // Sync wallet to see all the UTXOs
    {
        let mut wallet = maker.get_wallet().write().unwrap();
        wallet.sync_no_fail();
    }

    // Now test coin selection with manual_utxo_outpoints = [first two]
    let wallet = maker.get_wallet().read().unwrap();
    let manual = utxo_outpoints[..2].to_vec();

    let selected = wallet
        .coin_select(
            Amount::from_btc(0.25).unwrap(),
            MIN_FEE_RATE,
            Some(manual.clone()),
        )
        .unwrap();
    let selected_outpoints = selected
        .iter()
        .map(|(u, _)| OutPoint::new(u.txid, u.vout))
        .collect::<Vec<_>>();

    // The selected UTXOs should be exactly the manually specified ones
    assert_eq!(
        selected_outpoints, manual,
        "Coin selection did not honor manual_utxo_outpoints"
    );

    // Should fail if manual_utxo_outpoints can't cover the target
    let too_small = wallet
        .coin_select(Amount::from_btc(0.5).unwrap(), MIN_FEE_RATE, Some(manual))
        .err();
    assert!(
        too_small.is_some(),
        "Should fail if manual UTXOs can't cover target"
    );

    // Should succeed if all UTXOs are allowed
    let all = wallet
        .coin_select(Amount::from_btc(0.5).unwrap(), MIN_FEE_RATE, None)
        .unwrap();
    let total: Amount = all.iter().map(|(u, _)| u.amount).sum();
    assert!(total >= Amount::from_btc(0.5).unwrap());

    println!("✅ Manual UTXO coin selection test passed");
}

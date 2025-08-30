#![cfg(feature = "integration-test")]
use bitcoin::Amount;
use coinswap::utill::interactive_select;
mod test_framework;
use test_framework::*;

/// This test demonstrates the interactive UTXO selection functionality.
#[test]
fn test_interactive_utxo_selection() {
    let (test_framework, _, makers, _directory_server_instance, _block_generation_handle) =
        TestFramework::init(
            [((6102, None), coinswap::maker::MakerBehavior::Normal)].into(),
            vec![coinswap::taker::TakerBehavior::Normal],
            coinswap::utill::ConnectionType::CLEARNET,
        );

    let bitcoind = &test_framework.bitcoind;
    let maker = makers.first().unwrap();

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

    {
        let mut wallet = maker.get_wallet().write().unwrap();
        wallet.sync_no_fail();
    }

    let wallet = maker.get_wallet().read().unwrap();
    let all_utxos = wallet.get_all_utxo().unwrap();

    let selected = interactive_select(all_utxos.clone()).expect("interactive_select failed");

    println!("Selected UTXOs:");
    for utxo in selected.iter() {
        println!("  - {} BTC ({})", utxo.amount.to_btc(), utxo.txid);
    }

    let total_selected: Amount = selected.iter().map(|u| u.amount).sum();
    println!("Total selected amount: {} BTC", total_selected.to_btc());

    println!("✅ Interactive UTXO selection completed");
}

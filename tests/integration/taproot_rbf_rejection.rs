//! Integration test: maker rejects Taproot contract data while incoming contract
//! tx is still unconfirmed in mempool (issue #882 regression).

use bitcoin::Amount;
use coinswap::{
    maker::{start_server, MakerBehavior},
    protocol::common_messages::ProtocolVersion,
    taker::{SwapParams, TakerBehavior},
    wallet::AddressType,
};

use super::test_framework::*;

use log::{info, warn};
use std::{sync::atomic::Ordering::Relaxed, thread, time::Duration};

#[test]
fn test_maker_rejects_mempool_taproot_contract() {
    warn!("Running Test: Taproot RBF rejection — maker confirmation gate");

    // Ports verified unused (7102/17102=taproot_swap, 7202/17202=taproot_maker_abort2)
    let makers_config_map = vec![(8502, Some(21051)), (18502, Some(21052))];
    let taker_behavior = vec![TakerBehavior::SendMempoolTaprootContract];
    let maker_behaviors = vec![MakerBehavior::Normal, MakerBehavior::Normal];

    let (test_framework, mut takers, makers, block_generation_handle) =
        TestFramework::init(makers_config_map, taker_behavior, maker_behaviors);

    let bitcoind = &test_framework.bitcoind;
    let taker = takers.get_mut(0).unwrap();

    fund_taker(
        taker,
        bitcoind,
        3,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2TR,
    );
    fund_makers(
        &makers,
        bitcoind,
        4,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2TR,
    );

    let maker_threads: Vec<_> = makers
        .iter()
        .map(|maker| {
            let maker_clone = maker.clone();
            thread::spawn(move || start_server(maker_clone).unwrap())
        })
        .collect();

    wait_for_makers_setup(&makers, 120);
    for maker in &makers {
        maker.wallet.write().unwrap().sync_and_save().unwrap();
    }

    let maker_spendable_balance = verify_maker_pre_swap_balances(&makers);

    let swap_params = SwapParams::new(ProtocolVersion::Taproot, Amount::from_sat(500000), 2)
        .with_tx_count(3)
        .with_required_confirms(1);

    generate_blocks(bitcoind, 1);

    let summary = taker
        .prepare_coinswap(swap_params)
        .expect("prepare_coinswap should succeed");

    // Stop the background block generator so the taker contract tx stays
    // unconfirmed while the maker runs verify_contract_tx_on_chain.
    test_framework.pause_block_generation();
    thread::sleep(Duration::from_secs(4));

    let swap_result = taker.start_coinswap(&summary.swap_id);
    assert!(
        swap_result.is_err(),
        "Swap must fail: maker should reject unconfirmed incoming contract"
    );
    info!("Swap failed as expected: {:?}", swap_result.err().unwrap());

    makers.iter().for_each(|m| m.shutdown.store(true, Relaxed));
    for t in maker_threads {
        t.join().unwrap();
    }

    for (i, (maker, original)) in makers.iter().zip(maker_spendable_balance).enumerate() {
        maker.wallet.write().unwrap().sync_and_save().unwrap();
        let spendable = maker
            .wallet
            .read()
            .unwrap()
            .get_balances()
            .unwrap()
            .spendable;
        assert_eq!(
            spendable, original,
            "Maker {} spendable balance changed — outgoing side may have been funded",
            i
        );
    }

    test_framework.stop();
    block_generation_handle.join().unwrap();
}

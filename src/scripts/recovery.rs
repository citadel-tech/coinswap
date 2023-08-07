use crate::{
    error::TeleportError,
    protocol::contract::Hash160,
    wallet::{RPCConfig, Wallet, WalletSwapCoin},
};
use bitcoincore_rpc::RpcApi;
use std::path::PathBuf;

pub fn recover_from_incomplete_coinswap(
    wallet_file_name: &PathBuf,
    hashvalue: Hash160,
    dont_broadcast: bool,
) -> Result<(), TeleportError> {
    let mut wallet = Wallet::load(
        &RPCConfig::default(),
        wallet_file_name,
        None, /* Normal Mode */
    )?;
    wallet.sync()?;

    let incomplete_coinswaps = wallet.find_incomplete_coinswaps()?;
    let incomplete_coinswap = incomplete_coinswaps.get(&hashvalue);
    if incomplete_coinswap.is_none() {
        log::error!(target: "main", "hashvalue not refering to incomplete coinswap, run \
                `wallet-balance` to see list of incomplete coinswaps");
        return Ok(());
    }
    let incomplete_coinswap = incomplete_coinswap.unwrap();
    for (ii, swapcoin) in incomplete_coinswap
        .0
        .iter()
        .map(|(l, i)| (l, (*i as &dyn WalletSwapCoin)))
        .chain(
            incomplete_coinswap
                .1
                .iter()
                .map(|(l, o)| (l, (*o as &dyn WalletSwapCoin))),
        )
        .enumerate()
    {
        wallet
            .import_wallet_contract_redeemscript(&swapcoin.1.get_contract_redeemscript())
            .unwrap();

        let signed_contract_tx = swapcoin.1.get_fully_signed_contract_tx();
        if dont_broadcast {
            let txhex = bitcoin::consensus::encode::serialize_hex(&signed_contract_tx);
            println!(
                "contract_tx_{} (txid = {}) = \n{}",
                ii,
                signed_contract_tx.txid(),
                txhex
            );
            let accepted = wallet
                .rpc
                .test_mempool_accept(&[txhex.clone()])
                .unwrap()
                .iter()
                .any(|tma| tma.allowed);
            assert!(accepted);
        } else {
            let txid = wallet.rpc.send_raw_transaction(&signed_contract_tx)?;
            println!("broadcasted {}", txid);
        }
    }
    Ok(())
}

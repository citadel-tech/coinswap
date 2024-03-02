//! Some Wallet application scripts made with the Wallet API.
//!
//! These include generating or recovering a wallet, displaying wallet balances and addresses,
//! printing receive invoices, and performing direct send operations. The functions handle
//! wallet initialization, synchronization with a backend node, and various display and transaction operations.
//! The module provides a convenient interface for users interacting with the teleport wallet.

use crate::wallet::{SwapCoin, UTXOSpendInfo, Wallet, WalletStore};
use bitcoin::{consensus::encode::serialize_hex, Amount};
use bitcoind::bitcoincore_rpc::RpcApi;
use std::{convert::TryInto, path::PathBuf};

use bip39::Mnemonic;

use crate::wallet::{
    CoinToSpend, Destination, DisplayAddressType, RPCConfig, SendAmount, WalletError,
    WalletSwapCoin,
};

use crate::protocol::contract::read_contract_locktime;

use std::iter::repeat;

/// Some top level wrapper functions over Wallet API to perform various tasks.
/// These are used in the teleport-cli app.

/// Generate a wallet file with fresh seed and sync with a backend node.
/// This will fail if RPC connection and sync operation fails. So the RPC backend
/// should be reachable before this call.
///
/// This function can also be used to restore a wallet file with initial seed and passphrase.
///
// TODO: Remove these scripts. Remake them in actual wallet API if required.
pub fn generate_wallet(
    wallet_file: &PathBuf,
    rpc_config: Option<RPCConfig>,
) -> Result<(), WalletError> {
    let rpc_config = rpc_config.unwrap_or_default();

    println!("Input an optional passphrase (or leave blank for none): ");
    let mut passphrase = String::new();
    std::io::stdin().read_line(&mut passphrase)?;
    passphrase = passphrase.trim().to_string();
    let mnemonic = Mnemonic::generate(12)?;
    let mut wallet = Wallet::init(
        wallet_file,
        &rpc_config,
        mnemonic.to_string(),
        passphrase.clone(),
    )?;

    println!("Importing addresses into Core. . .");
    if let Err(e) = wallet.sync() {
        print!("Wallet syncing failed. Cleaning up wallet file");
        wallet.delete_wallet_file()?;
        return Err(e);
    }

    println!("Write down this seed phrase:\n{}", mnemonic);
    if !passphrase.trim().is_empty() {
        println!("And this passphrase:\n\"{}\"", passphrase);
    }
    println!(
        "\nThis seed phrase is NOT enough to backup all coins in your wallet.\n\
        The teleport wallet file is needed to backup swapcoins"
    );
    println!("\nSaved to file `{}`", wallet_file.to_string_lossy());

    Ok(())
}

/// Reset a wallet file with a given menmomic and passphrase
pub fn recover_wallet(wallet_file: &PathBuf) -> Result<(), WalletError> {
    println!("Input seed phrase: ");
    let mut seed_phrase = String::new();
    std::io::stdin().read_line(&mut seed_phrase)?;
    seed_phrase = seed_phrase.trim().to_string();

    if let Err(e) = Mnemonic::parse(&seed_phrase) {
        println!("Invalid seed phrase: {:?}", e);
        return Ok(());
    }

    println!("Input seed phrase extension (or leave blank for none): ");
    let mut passphrase = String::new();
    std::io::stdin().read_line(&mut passphrase)?;
    passphrase = passphrase.trim().to_string();

    let wallet_name = wallet_file
        .file_name()
        .expect("Filename expected!")
        .to_str()
        .unwrap()
        .to_string();

    // Init the store only, with regtest hard coded.
    // TODO: Specify Network. Handle unwrap.
    let _ = WalletStore::init(
        wallet_name,
        wallet_file,
        bitcoin::Network::Regtest,
        seed_phrase,
        passphrase,
    )
    .unwrap();
    println!("\nSaved to file `{}`", wallet_file.to_string_lossy());
    Ok(())
}

/// Display various kind of addresses and balances.
pub fn display_wallet_balance(
    wallet_file: &PathBuf,
    rpc_config: Option<RPCConfig>,
    long_form: Option<bool>,
) -> Result<(), WalletError> {
    let mut wallet = Wallet::load(&rpc_config.unwrap_or_default(), wallet_file)?;

    wallet.sync()?;

    let long_form = long_form.unwrap_or(false);

    let mut utxos_incl_fbonds = wallet.list_fidelity_unspend_from_wallet()?;
    let mut seed_coin_utxo = wallet.list_descriptor_utxo_unspend_from_wallet()?;
    let mut swap_coin_utxo = wallet.list_swap_coin_unspend_from_wallet()?;

    utxos_incl_fbonds.append(&mut seed_coin_utxo);
    utxos_incl_fbonds.append(&mut swap_coin_utxo);

    let (mut utxos, mut fidelity_bond_utxos): (Vec<_>, Vec<_>) =
        utxos_incl_fbonds.iter().partition(|(_, usi)| {
            !matches!(
                usi,
                UTXOSpendInfo::FidelityBondCoin {
                    index: _,
                    input_value: _,
                }
            )
        });

    utxos.sort_by(|(a, _), (b, _)| b.confirmations.cmp(&a.confirmations));
    let utxo_count = utxos.len();
    let balance: Amount = utxos
        .iter()
        .fold(Amount::ZERO, |acc, (u, _)| acc + u.amount);
    println!("= Spendable wallet balance =");
    println!(
        "{:16} {:24} {:^8} {:<7} value",
        "coin", "address", "type", "conf"
    );
    for (utxo, _) in utxos {
        let txid = utxo.txid.to_string();
        let addr = utxo.address.clone().unwrap().assume_checked().to_string();
        #[rustfmt::skip]
        println!(
            "{}{}{}:{} {}{}{} {:^8} {:<7} {}",
            if long_form { &txid } else {&txid[0..6] },
            if long_form { "" } else { ".." },
            if long_form { "" } else { &txid[58..64] },
            utxo.vout,
            if long_form { &addr } else { &addr[0..10] },
            if long_form { "" } else { "...." },
            if long_form { "" } else { &addr[addr.len() - 10..addr.len()] },
            if utxo.witness_script.is_some() {
                "swapcoin"
            } else if utxo.descriptor.is_some() { "seed" } else { "timelock" },
            utxo.confirmations,
            utxo.amount
        );
    }
    println!("Coin count = {}", utxo_count);
    println!("Total balance = {}", balance);

    let incomplete_coinswaps = wallet.find_incomplete_coinswaps()?;
    if !incomplete_coinswaps.is_empty() {
        println!("= Incomplete coinswaps =");
        for (hashvalue, (utxo_incoming_swapcoins, utxo_outgoing_swapcoins)) in incomplete_coinswaps
        {
            let incoming_swapcoins_balance: Amount = utxo_incoming_swapcoins
                .iter()
                .fold(Amount::ZERO, |acc, us| acc + us.1.amount);
            let outgoing_swapcoins_balance: Amount = utxo_outgoing_swapcoins
                .iter()
                .fold(Amount::ZERO, |acc, us| acc + us.1.amount);

            println!(
                "{:16} {:8} {:8} {:<15} {:<7} value",
                "coin", "type", "preimage", "locktime/blocks", "conf"
            );
            for ((utxo, swapcoin), contract_type) in utxo_incoming_swapcoins
                .iter()
                .map(|(i, l)| (l, *i as &dyn WalletSwapCoin))
                .zip(repeat("hashlock"))
                .chain(
                    utxo_outgoing_swapcoins
                        .iter()
                        .map(|(o, l)| (l, *o as &dyn WalletSwapCoin))
                        .zip(repeat("timelock")),
                )
            {
                let txid = serialize_hex(&utxo.txid);

                #[rustfmt::skip]
                println!("{}{}{}:{} {:8} {:8} {:^15} {:<7} {}",
                    if long_form { &txid } else {&txid[0..6] },
                    if long_form { "" } else { ".." },
                    if long_form { "" } else { &txid[58..64] },
                    utxo.vout,
                    contract_type,
                    if swapcoin.is_hash_preimage_known() { "known" } else { "unknown" },
                    read_contract_locktime(&swapcoin.get_contract_redeemscript())
                        .expect("unable to read locktime from contract"),
                    utxo.confirmations,
                    utxo.amount
                );
            }
            if incoming_swapcoins_balance != Amount::ZERO {
                println!(
                    "Amount earned if coinswap is successful = {}",
                    incoming_swapcoins_balance.to_signed().unwrap()
                        - outgoing_swapcoins_balance.to_signed().unwrap()
                );
            }
            println!(
                "Outgoing balance = {}\nhashvalue = {}",
                outgoing_swapcoins_balance, hashvalue
            );
        }
    }

    let (mut incoming_contract_utxos, mut outgoing_contract_utxos) =
        wallet.find_live_contract_unspents()?;
    if !outgoing_contract_utxos.is_empty() {
        outgoing_contract_utxos.sort_by(|a, b| b.1.confirmations.cmp(&a.1.confirmations));
        println!("= Live timelocked contracts =");
        println!(
            "{:16} {:10} {:8} {:<7} {:<8} {:6}",
            "coin", "hashvalue", "timelock", "conf", "locked?", "value"
        );
        for (outgoing_swapcoin, utxo) in outgoing_contract_utxos {
            let txid = utxo.txid.to_string();
            let timelock =
                read_contract_locktime(&outgoing_swapcoin.contract_redeemscript).unwrap();
            let hashvalue = outgoing_swapcoin.get_hashvalue().to_string();
            #[rustfmt::skip]
            println!("{}{}{}:{} {}{} {:<8} {:<7} {:<8} {}",
                if long_form { &txid } else {&txid[0..6] },
                if long_form { "" } else { ".." },
                if long_form { "" } else { &txid[58..64] },
                utxo.vout,
                if long_form { &hashvalue } else { &hashvalue[..8] },
                if long_form { "" } else { ".." },
                timelock,
                utxo.confirmations,
                if utxo.confirmations >= timelock.into() { "unlocked" } else { "locked" },
                utxo.amount
            );
        }
    }

    //ordinary users shouldnt be spending via the hashlock branch
    //maybe makers since they're a bit more expertly, and they dont start with the hash preimage
    //but takers should basically never use the hash preimage
    let expert_mode = true;
    if expert_mode && !incoming_contract_utxos.is_empty() {
        incoming_contract_utxos.sort_by(|a, b| b.1.confirmations.cmp(&a.1.confirmations));
        println!("= Live hashlocked contracts =");
        println!(
            "{:16} {:10} {:8} {:<7} {:8} {:6}",
            "coin", "hashvalue", "timelock", "conf", "preimage", "value"
        );
        for (incoming_swapcoin, utxo) in incoming_contract_utxos {
            let txid = utxo.txid.to_string();
            let timelock =
                read_contract_locktime(&incoming_swapcoin.contract_redeemscript).unwrap();
            let hashvalue = incoming_swapcoin.get_hashvalue().to_string();
            #[rustfmt::skip]
            println!("{}{}{}:{} {}{} {:<8} {:<7} {:8} {}",
                if long_form { &txid } else {&txid[0..6] },
                if long_form { "" } else { ".." },
                if long_form { "" } else { &txid[58..64] },
                utxo.vout,
                if long_form { &hashvalue } else { &hashvalue[..8] },
                if long_form { "" } else { ".." },
                timelock,
                utxo.confirmations,
                if incoming_swapcoin.is_hash_preimage_known() { "known" } else { "unknown" },
                utxo.amount
            );
        }
    }

    if !fidelity_bond_utxos.is_empty() {
        println!("= Fidelity bond coins =");
        println!(
            "{:16} {:24} {:<7} {:<11} {:<8} {:6}",
            "coin", "address", "conf", "locktime", "locked?", "value"
        );

        let mediantime = wallet.rpc.get_blockchain_info().unwrap().median_time;
        fidelity_bond_utxos.sort_by(|(a, _), (b, _)| b.confirmations.cmp(&a.confirmations));
        for (utxo, utxo_spend_info) in fidelity_bond_utxos {
            if !matches!(utxo_spend_info, UTXOSpendInfo::FidelityBondCoin { .. }) {
                panic!("Logic error, all these UTXOs should be fidelity bonds!");
            }

            let unix_locktime = 123456789; // yes its a bug. Doesn't need to be fixed.
            let txid = utxo.txid.to_string();
            let addr = utxo.address.clone().unwrap().assume_checked().to_string();
            #[rustfmt::skip]
            println!(
                "{}{}{}:{} {}{}{} {:<7} {:<11} {:<8} {:6}",
                if long_form { &txid } else {&txid[0..6] },
                if long_form { "" } else { ".." },
                if long_form { "" } else { &txid[58..64] },
                utxo.vout,
                if long_form { &addr } else { &addr[0..10] },
                if long_form { "" } else { "...." },
                if long_form { "" } else { &addr[addr.len() - 10..addr.len()] },
                utxo.confirmations,
                unix_locktime,
                if mediantime >= unix_locktime.try_into().unwrap() { "unlocked" } else { "locked" },
                utxo.amount
            );
        }
    }

    Ok(())
}

/// Display basic wallet balances.
pub fn display_wallet_addresses(
    wallet_file_name: &PathBuf,
    types: DisplayAddressType,
) -> Result<(), WalletError> {
    let wallet = Wallet::load(&RPCConfig::default(), wallet_file_name)?;
    wallet.display_addresses(types)?;
    Ok(())
}

/// Prints the receive invoice for the wallet.
pub fn print_receive_invoice(wallet_file_name: &PathBuf) -> Result<(), WalletError> {
    let mut wallet = Wallet::load(&RPCConfig::default(), wallet_file_name)?;
    wallet.sync()?;

    let addr = wallet.get_next_external_address()?;
    println!("{}", addr);

    Ok(())
}

/// Perform a direct send operation.
pub fn direct_send(
    wallet_file_name: &PathBuf,
    fee_rate: u64,
    send_amount: SendAmount,
    destination: Destination,
    coins_to_spend: &[CoinToSpend],
    dont_broadcast: bool,
) -> Result<(), WalletError> {
    let mut wallet = Wallet::load(&RPCConfig::default(), wallet_file_name)?;
    wallet.sync()?;
    let tx = wallet
        .create_direct_send(fee_rate, send_amount, destination, coins_to_spend)
        .unwrap();
    let txhex = bitcoin::consensus::encode::serialize_hex(&tx);
    log::debug!("Fully signed tx hex = {}", txhex);
    let test_mempool_accept_result = &wallet.rpc.test_mempool_accept(&[txhex.clone()]).unwrap()[0];
    if !test_mempool_accept_result.allowed {
        panic!(
            "Created invalid transaction, reason = {:#?}",
            test_mempool_accept_result
        );
    }
    println!(
        "Actual fee rate = {:.3} sat/vb",
        (test_mempool_accept_result
            .fees
            .as_ref()
            .unwrap()
            .base
            .to_sat() as f64)
            / (test_mempool_accept_result.vsize.unwrap() as f64)
    );
    if dont_broadcast {
        println!("Tx = \n{}", txhex);
    } else {
        let txid = wallet.rpc.send_raw_transaction(&tx).unwrap();
        println!("Broadcasted {}", txid);
    }
    Ok(())
}

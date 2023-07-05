use std::io::ErrorKind;

use bitcoin::{
    secp256k1::{SecretKey, Signature},
    OutPoint, PublicKey, Script, Transaction,
};
use bitcoincore_rpc::Client;
use itertools::izip;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::tcp::{ReadHalf, WriteHalf},
};

use crate::{
    contracts::{
        self, create_receivers_contract_tx, find_funding_output,
        read_pubkeys_from_multisig_redeemscript, sign_contract_tx, SwapCoin, WatchOnlySwapCoin,
    },
    error::Error,
    messages::{
        ContractSigsAsRecvrAndSender, MakerToTakerMessage, MultisigPrivkey, Preimage,
        TakerToMakerMessage,
    },
    offerbook_sync::OfferAndAddress,
    wallet_sync::{import_watchonly_redeemscript, IncomingSwapCoin, OutgoingSwapCoin, Wallet},
};

/// Chose the next Maker who's offer amount range satisfies the given amount.
pub fn choose_next_maker(
    maker_offers_addresses: &mut Vec<OfferAndAddress>,
    amount: u64,
) -> Option<OfferAndAddress> {
    loop {
        let m = maker_offers_addresses.pop()?;
        if amount < m.offer.min_size || amount > m.offer.max_size {
            log::debug!("amount out of range for maker = {:?}", m);
            continue;
        }
        log::debug!("next maker = {:?}", m);
        break Some(m);
    }
}

/// Send message to a Maker.
pub async fn send_message(
    socket_writer: &mut WriteHalf<'_>,
    message: TakerToMakerMessage,
) -> Result<(), Error> {
    log::debug!("==> {:#?}", message);
    let mut result_bytes = serde_json::to_vec(&message).map_err(|e| std::io::Error::from(e))?;
    result_bytes.push(b'\n');
    socket_writer.write_all(&result_bytes).await?;
    Ok(())
}

/// Read a Maker Message
pub async fn read_message(
    reader: &mut BufReader<ReadHalf<'_>>,
) -> Result<MakerToTakerMessage, Error> {
    let mut line = String::new();
    let n = reader.read_line(&mut line).await?;
    if n == 0 {
        return Err(Error::Network(Box::new(std::io::Error::new(
            ErrorKind::ConnectionReset,
            "EOF",
        ))));
    }
    let message: MakerToTakerMessage = match serde_json::from_str(&line) {
        Ok(r) => r,
        Err(_e) => return Err(Error::Protocol("json parsing error")),
    };
    log::debug!("<== {:#?}", message);
    Ok(message)
}

//TODO: This should be Wallet API.
pub fn sign_receivers_contract_txs(
    receivers_contract_txes: &[Transaction],
    outgoing_swapcoins: &[OutgoingSwapCoin],
) -> Result<Vec<Signature>, Error> {
    receivers_contract_txes
        .iter()
        .zip(outgoing_swapcoins.iter())
        .map(|(receivers_contract_tx, outgoing_swapcoin)| {
            outgoing_swapcoin.sign_contract_tx_with_my_privkey(receivers_contract_tx)
        })
        .collect::<Result<Vec<Signature>, Error>>()
}

//TODO: This Should be a wallet API.
pub fn sign_senders_contract_txs(
    my_receiving_multisig_privkeys: &[SecretKey],
    maker_sign_sender_and_receiver_contracts: &ContractSigsAsRecvrAndSender,
) -> Result<Vec<Signature>, Error> {
    my_receiving_multisig_privkeys
        .iter()
        .zip(
            maker_sign_sender_and_receiver_contracts
                .senders_contract_txs_info
                .iter(),
        )
        .map(
            |(my_receiving_multisig_privkey, senders_contract_tx_info)| {
                sign_contract_tx(
                    &senders_contract_tx_info.contract_tx,
                    &senders_contract_tx_info.multisig_redeemscript,
                    senders_contract_tx_info.funding_amount,
                    my_receiving_multisig_privkey,
                )
            },
        )
        .collect::<Result<Vec<Signature>, bitcoin::secp256k1::Error>>()
        .map_err(|_| Error::Protocol("error with signing contract tx"))
}

// TODO: This should be a Wallet API.
pub fn create_watch_only_swapcoins(
    rpc: &Client,
    maker_sign_sender_and_receiver_contracts: &ContractSigsAsRecvrAndSender,
    next_peer_multisig_pubkeys: &[PublicKey],
    next_swap_contract_redeemscripts: &[Script],
) -> Result<Vec<WatchOnlySwapCoin>, Error> {
    let next_swapcoins = izip!(
        maker_sign_sender_and_receiver_contracts
            .senders_contract_txs_info
            .iter(),
        next_peer_multisig_pubkeys.iter(),
        next_swap_contract_redeemscripts.iter()
    )
    .map(
        |(senders_contract_tx_info, &maker_multisig_pubkey, contract_redeemscript)| {
            WatchOnlySwapCoin::new(
                &senders_contract_tx_info.multisig_redeemscript,
                maker_multisig_pubkey,
                senders_contract_tx_info.contract_tx.clone(),
                contract_redeemscript.clone(),
                senders_contract_tx_info.funding_amount,
            )
        },
    )
    .collect::<Result<Vec<WatchOnlySwapCoin>, Error>>()?;
    //TODO error handle here the case where next_swapcoin.contract_tx script pubkey
    // is not equal to p2wsh(next_swap_contract_redeemscripts)
    for swapcoin in &next_swapcoins {
        import_watchonly_redeemscript(rpc, &swapcoin.get_multisig_redeemscript())?
    }
    Ok(next_swapcoins)
}

// TODO: This should be a Wallet API.
pub fn create_incoming_swapcoins(
    rpc: &Client,
    wallet: &Wallet,
    maker_sign_sender_and_receiver_contracts: &ContractSigsAsRecvrAndSender,
    funding_txes: &[Transaction],
    funding_tx_merkleproofs: &[String],
    next_swap_info: &crate::taker::NextSwapInfo,
    preimage: &Preimage,
) -> Result<Vec<IncomingSwapCoin>, Error> {
    let next_swap_multisig_redeemscripts = maker_sign_sender_and_receiver_contracts
        .senders_contract_txs_info
        .iter()
        .map(|senders_contract_tx_info| senders_contract_tx_info.multisig_redeemscript.clone())
        .collect::<Vec<Script>>();
    let next_swap_funding_outpoints = maker_sign_sender_and_receiver_contracts
        .senders_contract_txs_info
        .iter()
        .map(|senders_contract_tx_info| {
            senders_contract_tx_info.contract_tx.input[0].previous_output
        })
        .collect::<Vec<OutPoint>>();

    let last_makers_funding_tx_values = funding_txes
        .iter()
        .zip(next_swap_multisig_redeemscripts.iter())
        .map(|(makers_funding_tx, multisig_redeemscript)| {
            find_funding_output(&makers_funding_tx, &multisig_redeemscript)
                .ok_or(Error::Protocol(
                    "multisig redeemscript not found in funding tx",
                ))
                .map(|txout| txout.1.value)
        })
        .collect::<Result<Vec<u64>, Error>>()?;
    let my_receivers_contract_txes = izip!(
        next_swap_funding_outpoints.iter(),
        last_makers_funding_tx_values.iter(),
        next_swap_info.contract_reedemscripts.iter()
    )
    .map(
        |(&previous_funding_output, &maker_funding_tx_value, next_contract_redeemscript)| {
            create_receivers_contract_tx(
                previous_funding_output,
                maker_funding_tx_value,
                next_contract_redeemscript,
            )
        },
    )
    .collect::<Vec<Transaction>>();

    let mut incoming_swapcoins = Vec::<IncomingSwapCoin>::new();
    for (
        multisig_redeemscript,
        &maker_funded_multisig_pubkey,
        &maker_funded_multisig_privkey,
        my_receivers_contract_tx,
        next_contract_redeemscript,
        &hashlock_privkey,
        &maker_funding_tx_value,
        funding_tx,
        funding_tx_merkleproof,
    ) in izip!(
        next_swap_multisig_redeemscripts.iter(),
        next_swap_info.multisig_pubkeys.iter(),
        next_swap_info.multisig_nonces.iter(),
        my_receivers_contract_txes.iter(),
        next_swap_info.contract_reedemscripts.iter(),
        next_swap_info.hashlock_nonces.iter(),
        last_makers_funding_tx_values.iter(),
        funding_txes.iter(),
        funding_tx_merkleproofs.iter(),
    ) {
        let (o_ms_pubkey1, o_ms_pubkey2) =
            read_pubkeys_from_multisig_redeemscript(multisig_redeemscript)
                .ok_or(Error::Protocol("invalid pubkeys in multisig redeemscript"))?;
        let maker_funded_other_multisig_pubkey = if o_ms_pubkey1 == maker_funded_multisig_pubkey {
            o_ms_pubkey2
        } else {
            if o_ms_pubkey2 != maker_funded_multisig_pubkey {
                return Err(Error::Protocol("maker-funded multisig doesnt match"));
            }
            o_ms_pubkey1
        };

        wallet.import_wallet_multisig_redeemscript(&rpc, &o_ms_pubkey1, &o_ms_pubkey2)?;
        wallet.import_tx_with_merkleproof(&rpc, funding_tx, funding_tx_merkleproof.clone())?;
        wallet.import_wallet_contract_redeemscript(rpc, &next_contract_redeemscript)?;

        let mut incoming_swapcoin = IncomingSwapCoin::new(
            maker_funded_multisig_privkey,
            maker_funded_other_multisig_pubkey,
            my_receivers_contract_tx.clone(),
            next_contract_redeemscript.clone(),
            hashlock_privkey,
            maker_funding_tx_value,
        );
        incoming_swapcoin.hash_preimage = Some(*preimage);
        incoming_swapcoins.push(incoming_swapcoin);
    }

    Ok(incoming_swapcoins)
}

//TODO: This should be wallet API.
//TODO: The checking part is missing. Add the check. probably this should be added into the trait of SwapCoin.
pub fn check_and_apply_maker_private_keys<S: SwapCoin>(
    swapcoins: &mut Vec<S>,
    swapcoin_private_keys: &[MultisigPrivkey],
) -> Result<(), Error> {
    for (swapcoin, swapcoin_private_key) in swapcoins.iter_mut().zip(swapcoin_private_keys.iter()) {
        swapcoin
            .apply_privkey(swapcoin_private_key.key)
            .map_err(|_| Error::Protocol("wrong privkey"))?;
    }
    Ok(())
}

/// Generate The Maker's Multisig and HashLock keys and respective nonce values.
/// Nonce values are random integers and resulting Pubkeys are derived by tweaking the
/// Make's advertised Pubkey with these two nonces.
pub fn generate_maker_keys(
    tweakable_point: &PublicKey,
    count: u32,
) -> (
    Vec<PublicKey>,
    Vec<SecretKey>,
    Vec<PublicKey>,
    Vec<SecretKey>,
) {
    let (multisig_pubkeys, multisig_nonces): (Vec<_>, Vec<_>) = (0..count)
        .map(|_| contracts::derive_maker_pubkey_and_nonce(*tweakable_point).unwrap())
        .unzip();
    let (hashlock_pubkeys, hashlock_nonces): (Vec<_>, Vec<_>) = (0..count)
        .map(|_| contracts::derive_maker_pubkey_and_nonce(*tweakable_point).unwrap())
        .unzip();
    (
        multisig_pubkeys,
        multisig_nonces,
        hashlock_pubkeys,
        hashlock_nonces,
    )
}

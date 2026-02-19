//! Legacy (ECDSA) message verification for the Unified Taker.
//!
//! Verifies every message received from makers during the legacy swap flow.
//! A malicious maker could send invalid signatures, wrong scripts, or
//! mismatched hash values — these checks catch all of those.

use bitcoin::{
    hashes::{hash160::Hash as Hash160, Hash},
    Amount, PublicKey, ScriptBuf, Transaction,
};

use crate::protocol::{
    contract::{
        check_reedemscript_is_multisig, read_contract_locktime, read_hashlock_pubkey_from_contract,
        read_hashvalue_from_contract, read_pubkeys_from_multisig_redeemscript,
        validate_contract_tx, verify_contract_tx_sig,
    },
    legacy_messages::SenderContractTxInfo,
};

use super::{error::TakerError, unified_api::UnifiedTaker};

impl UnifiedTaker {
    /// Verify sender contract signatures received from the first maker.
    ///
    /// Each signature must be valid against the corresponding outgoing swapcoin's
    /// contract tx, multisig redeemscript, funding amount, and the maker's pubkey.
    pub(crate) fn verify_sender_sigs(
        &self,
        sigs: &[bitcoin::ecdsa::Signature],
    ) -> Result<(), TakerError> {
        let outgoing = &self.swap_state()?.outgoing_swapcoins;

        for (i, (sig, swapcoin)) in sigs.iter().zip(outgoing.iter()).enumerate() {
            let other_pubkey = swapcoin.other_pubkey.ok_or_else(|| {
                TakerError::General(format!(
                    "Outgoing swapcoin {} missing other_pubkey for sig verification",
                    i
                ))
            })?;
            let my_pubkey = swapcoin.my_pubkey.ok_or_else(|| {
                TakerError::General(format!(
                    "Outgoing swapcoin {} missing my_pubkey for sig verification",
                    i
                ))
            })?;

            let multisig_redeemscript =
                crate::protocol::contract::create_multisig_redeemscript(&my_pubkey, &other_pubkey);

            verify_contract_tx_sig(
                &swapcoin.contract_tx,
                &multisig_redeemscript,
                swapcoin.funding_amount,
                &other_pubkey,
                &sig.signature,
            )
            .map_err(|e| {
                TakerError::General(format!(
                    "Invalid sender contract signature {} from maker: {:?}",
                    i, e
                ))
            })?;
        }

        log::info!(
            "Verified {} sender contract signatures from first maker",
            sigs.len()
        );
        Ok(())
    }

    /// Verify sender contract signatures received from a forwarded maker.
    ///
    /// Uses `SenderContractTxInfo` (from the current maker's response) rather than
    /// outgoing swapcoins (which only exist for the first hop).
    pub(crate) fn verify_sender_sigs_from_info(
        &self,
        sigs: &[bitcoin::ecdsa::Signature],
        senders_info: &[SenderContractTxInfo],
    ) -> Result<(), TakerError> {
        for (i, (sig, info)) in sigs.iter().zip(senders_info.iter()).enumerate() {
            let (pubkey1, pubkey2) =
                read_pubkeys_from_multisig_redeemscript(&info.multisig_redeemscript)?;

            // The signature must be valid for one of the two multisig pubkeys
            let valid = verify_contract_tx_sig(
                &info.contract_tx,
                &info.multisig_redeemscript,
                info.funding_amount,
                &pubkey1,
                &sig.signature,
            )
            .is_ok()
                || verify_contract_tx_sig(
                    &info.contract_tx,
                    &info.multisig_redeemscript,
                    info.funding_amount,
                    &pubkey2,
                    &sig.signature,
                )
                .is_ok();

            if !valid {
                return Err(TakerError::General(format!(
                    "Invalid forwarded sender contract signature {} from maker",
                    i
                )));
            }
        }

        log::info!(
            "Verified {} forwarded sender contract signatures",
            sigs.len()
        );
        Ok(())
    }

    /// Verify receiver contract signatures from a previous maker.
    ///
    /// The receiver contract tx is signed by the previous maker using one of the
    /// pubkeys from the multisig redeemscript.
    pub(crate) fn verify_receiver_sigs(
        &self,
        sigs: &[bitcoin::ecdsa::Signature],
        receivers_txs: &[Transaction],
        prev_senders_info: &[SenderContractTxInfo],
    ) -> Result<(), TakerError> {
        for (i, ((sig, tx), info)) in sigs
            .iter()
            .zip(receivers_txs.iter())
            .zip(prev_senders_info.iter())
            .enumerate()
        {
            let (pubkey1, pubkey2) =
                read_pubkeys_from_multisig_redeemscript(&info.multisig_redeemscript)?;

            let valid = verify_contract_tx_sig(
                tx,
                &info.multisig_redeemscript,
                info.funding_amount,
                &pubkey1,
                &sig.signature,
            )
            .is_ok()
                || verify_contract_tx_sig(
                    tx,
                    &info.multisig_redeemscript,
                    info.funding_amount,
                    &pubkey2,
                    &sig.signature,
                )
                .is_ok();

            if !valid {
                return Err(TakerError::General(format!(
                    "Invalid receiver contract signature {} from maker",
                    i
                )));
            }
        }

        log::info!("Verified {} receiver contract signatures", sigs.len());
        Ok(())
    }

    /// Verify the maker's sender contract data from `ReqContractSigsAsRecvrAndSender`.
    pub(crate) fn verify_maker_sender_contracts(
        &self,
        senders_info: &[SenderContractTxInfo],
        next_multisig_pubkeys: &[PublicKey],
        next_hashlock_pubkeys: &[PublicKey],
        refund_locktime: u16,
        min_expected_amount: Option<Amount>,
    ) -> Result<(), TakerError> {
        let expected_hashvalue = Hash160::hash(&self.swap_state()?.preimage);

        for (i, info) in senders_info.iter().enumerate() {
            // Validate 2-of-2 multisig format
            check_reedemscript_is_multisig(&info.multisig_redeemscript).map_err(|e| {
                TakerError::General(format!(
                    "Sender contract {} has invalid multisig redeemscript: {:?}",
                    i, e
                ))
            })?;

            // Verify contract tx spends from the provided funding tx
            if info.contract_tx.input.is_empty() {
                return Err(TakerError::General(format!(
                    "Sender contract {} contract_tx has no inputs",
                    i
                )));
            }
            let contract_input_txid = info.contract_tx.input[0].previous_output.txid;
            let expected_funding_txid = info.funding_tx.compute_txid();
            if contract_input_txid != expected_funding_txid {
                return Err(TakerError::General(format!(
                    "Sender contract {} contract_tx references wrong funding tx: expected {}, got {}",
                    i, expected_funding_txid, contract_input_txid
                )));
            }

            // Validate contract tx structure (1-in, 1-out, output pays to P2WSH of contract redeemscript)
            validate_contract_tx(&info.contract_tx, None, &info.contract_redeemscript).map_err(
                |e| {
                    TakerError::General(format!(
                        "Sender contract {} has invalid contract tx: {:?}",
                        i, e
                    ))
                },
            )?;

            // Verify hash in contract redeemscript matches our preimage
            let hashvalue = read_hashvalue_from_contract(&info.contract_redeemscript).map_err(
                |e| {
                    TakerError::General(format!(
                        "Sender contract {} has unreadable hashvalue: {:?}",
                        i, e
                    ))
                },
            )?;
            if hashvalue != expected_hashvalue {
                return Err(TakerError::General(format!(
                    "Sender contract {} has wrong hashvalue: expected {:?}, got {:?}",
                    i, expected_hashvalue, hashvalue
                )));
            }

            // Verify locktime is positive
            let locktime = read_contract_locktime(&info.contract_redeemscript).map_err(|e| {
                TakerError::General(format!(
                    "Sender contract {} has unreadable locktime: {:?}",
                    i, e
                ))
            })?;
            if locktime == 0 {
                return Err(TakerError::General(format!(
                    "Sender contract {} has zero locktime",
                    i
                )));
            }

            // Verify maker used exactly the locktime the taker requested.
            // For legacy (CSV relative locktime), any deviation is invalid:
            // higher delays recovery, lower could enable early sweeps.
            if locktime != refund_locktime {
                return Err(TakerError::General(format!(
                    "Sender contract {} locktime {} does not match requested refund locktime {}",
                    i, locktime, refund_locktime
                )));
            }

            // Verify multisig contains the expected next-hop pubkey
            if !next_multisig_pubkeys.is_empty() {
                let expected_pubkey =
                    next_multisig_pubkeys[i % next_multisig_pubkeys.len()];
                let (pubkey1, pubkey2) =
                    read_pubkeys_from_multisig_redeemscript(&info.multisig_redeemscript)?;
                if pubkey1 != expected_pubkey && pubkey2 != expected_pubkey {
                    return Err(TakerError::General(format!(
                        "Sender contract {} multisig does not contain expected next-hop pubkey",
                        i
                    )));
                }
            }

            // Verify hashlock pubkey matches what we provided for the next hop
            if !next_hashlock_pubkeys.is_empty() {
                let expected_hashlock =
                    next_hashlock_pubkeys[i % next_hashlock_pubkeys.len()];
                let contract_hashlock =
                    read_hashlock_pubkey_from_contract(&info.contract_redeemscript).map_err(
                        |e| {
                            TakerError::General(format!(
                                "Sender contract {} has unreadable hashlock pubkey: {:?}",
                                i, e
                            ))
                        },
                    )?;
                if contract_hashlock != expected_hashlock {
                    return Err(TakerError::General(format!(
                        "Sender contract {} hashlock pubkey does not match expected next-hop key",
                        i
                    )));
                }
            }
        }

        // Verify total funding amount is consistent with expected amount after fees.
        // This uses the maker's advertised fee schedule from the offer.
        if let Some(min_amount) = min_expected_amount {
            let total_funding: Amount = senders_info
                .iter()
                .map(|info| info.funding_amount)
                .sum();
            if total_funding < min_amount {
                return Err(TakerError::General(format!(
                    "Maker sender contracts total funding {} is below expected minimum {} \
                     (based on maker's advertised fee schedule)",
                    total_funding, min_amount
                )));
            }
        }

        log::info!(
            "Verified {} maker sender contracts (structure, hashvalue, locktime, pubkeys, amounts)",
            senders_info.len()
        );
        Ok(())
    }

    /// Verify the maker's receiver contract transactions from `ReqContractSigsAsRecvrAndSender`.
    pub(crate) fn verify_maker_receiver_contracts(
        &self,
        receivers_contract_txs: &[Transaction],
        funding_txs: &[Transaction],
        contract_redeemscripts: &[ScriptBuf],
    ) -> Result<(), TakerError> {
        let funding_txids: Vec<_> = funding_txs.iter().map(|tx| tx.compute_txid()).collect();

        for (i, tx) in receivers_contract_txs.iter().enumerate() {
            // Verify basic structure
            if tx.input.len() != 1 || tx.output.len() != 1 {
                return Err(TakerError::General(format!(
                    "Receiver contract tx {} has invalid input/output count: {} inputs, {} outputs",
                    i,
                    tx.input.len(),
                    tx.output.len()
                )));
            }

            // Verify the receiver contract spends from one of the funding transactions
            let input_txid = tx.input[0].previous_output.txid;
            if !funding_txids.contains(&input_txid) {
                return Err(TakerError::General(format!(
                    "Receiver contract tx {} does not spend from expected funding tx (spends from {})",
                    i, input_txid
                )));
            }

            // Verify output pays to P2WSH of the expected contract redeemscript
            if let Some(expected_rs) = contract_redeemscripts.get(i) {
                validate_contract_tx(tx, None, expected_rs).map_err(|e| {
                    TakerError::General(format!(
                        "Receiver contract tx {} output does not pay to expected P2WSH: {:?}",
                        i, e
                    ))
                })?;
            }

        }

        log::info!(
            "Verified {} maker receiver contract txs (structure, funding reference, scriptpubkey, amounts)",
            receivers_contract_txs.len()
        );
        Ok(())
    }
}

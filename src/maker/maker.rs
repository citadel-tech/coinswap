use std::{path::PathBuf, sync::RwLock};

use bitcoin::{
    secp256k1::{self, Secp256k1, Signature},
    OutPoint, PublicKey, Transaction,
};
use bitcoincore_rpc::RpcApi;

use crate::{
    protocol::{contract::check_hashvalues_are_equal, messages::ReqContractSigsForSender, Hash160},
    wallet::{RPCConfig, WalletMode},
};

use crate::{
    protocol::{
        contract::{
            check_hashlock_has_pubkey, check_multisig_has_pubkey, check_reedemscript_is_multisig,
            find_funding_output_index, read_contract_locktime, redeemscript_to_scriptpubkey,
        },
        messages::ProofOfFunding,
    },
    wallet::{IncomingSwapCoin, OutgoingSwapCoin, Wallet, WalletError},
};

use super::{config::MakerConfig, error::MakerError};

//used to configure the maker do weird things for testing
#[derive(Debug, Clone, Copy)]
pub enum MakerBehavior {
    Normal,
    CloseOnSignSendersContractTx,
}
/// A structure denoting expectation of type of taker message.
/// Used in the [ConnectionState] structure.
///
/// If the received message doesn't match expected message,
/// a protocol error will be returned.
#[derive(Debug, Default, PartialEq)]
pub enum ExpectedMessage {
    #[default]
    TakerHello,
    NewlyConnectedTaker,
    ReqContractSigsForSender,
    ProofOfFunding,
    ProofOfFundingORContractSigsForRecvrAndSender,
    ReqContractSigsForRecvr,
    HashPreimage,
    PrivateKeyHandover,
}

/// Per connection state maintaining list of swapcoins and next [ExpectedMessage]
#[derive(Debug, Default)]
pub struct ConnectionState {
    pub allowed_message: ExpectedMessage,
    pub incoming_swapcoins: Vec<IncomingSwapCoin>,
    pub outgoing_swapcoins: Vec<OutgoingSwapCoin>,
    pub pending_funding_txes: Vec<Transaction>,
}

/// The Maker Structure
pub struct Maker {
    /// Defines special maker behavior, only applicable for testing
    pub behavior: MakerBehavior,
    /// Maker configurations
    pub config: MakerConfig,
    /// Maker's underlying wallet
    pub wallet: RwLock<Wallet>,
    /// A flag to trigger shutdown event
    pub shutdown: RwLock<bool>,
}

impl Maker {
    /// Initialize a Maker structure, with a given wallet file path, rpc configuration,
    /// listening ort, onion address, wallet and special maker behavior.
    pub fn init(
        wallet_file_name: &PathBuf,
        rpc_config: &RPCConfig,
        port: u16,
        onion_addrs: String,
        wallet_mode: Option<WalletMode>,
        behavior: MakerBehavior,
    ) -> Result<Self, MakerError> {
        let mut wallet = Wallet::load(&rpc_config, wallet_file_name, wallet_mode)?;
        wallet.sync()?;
        Ok(Self {
            behavior,
            config: MakerConfig::init(port, onion_addrs),
            wallet: RwLock::new(wallet),
            shutdown: RwLock::new(false),
        })
    }

    /// Strigger shutdown
    pub fn shutdown(&self) -> Result<(), MakerError> {
        let mut flag = self.shutdown.write()?;
        *flag = true;
        Ok(())
    }

    /// Checks consistency of the [ProofOfFunding] message and return the Hashvalue
    /// used in hashlock transaction.
    pub fn verify_proof_of_funding(&self, message: &ProofOfFunding) -> Result<Hash160, MakerError> {
        if message.confirmed_funding_txes.len() == 0 {
            return Err(MakerError::General("No funding txs provided by Taker"));
        }

        for funding_info in &message.confirmed_funding_txes {
            //check that the funding transaction pays to correct multisig
            log::debug!(
                "Proof of Funding: \ntx = {:#?}\nMultisig_Reedimscript = {:x}",
                funding_info.funding_tx,
                funding_info.multisig_redeemscript
            );
            // check that the new locktime is sufficently short enough compared to the
            // locktime in the provided funding tx
            let locktime = read_contract_locktime(&funding_info.contract_redeemscript)?;
            if locktime - message.next_locktime < self.config.min_contract_reaction_time {
                return Err(MakerError::General(
                    "Next hop locktime too close to current hop locktime",
                ));
            }

            let funding_output_index = find_funding_output_index(funding_info)?;

            //check the funding_tx is confirmed confirmed to required depth
            if let Some(txout) = self
                .wallet
                .read()?
                .rpc
                .get_tx_out(&funding_info.funding_tx.txid(), funding_output_index, None)
                .map_err(WalletError::Rpc)?
            {
                if txout.confirmations < self.config.required_confirms as u32 {
                    return Err(MakerError::General(
                        "funding tx not confirmed to required depth",
                    ));
                }
            } else {
                return Err(MakerError::General("funding tx output doesnt exist"));
            }

            check_reedemscript_is_multisig(&funding_info.multisig_redeemscript)?;

            let (_, tweabale_pubkey) = self.wallet.read()?.get_tweakable_keypair();

            check_multisig_has_pubkey(
                &funding_info.multisig_redeemscript,
                &tweabale_pubkey,
                &funding_info.multisig_nonce,
            )?;

            check_hashlock_has_pubkey(
                &funding_info.contract_redeemscript,
                &tweabale_pubkey,
                &funding_info.hashlock_nonce,
            )?;

            //check that the provided contract matches the scriptpubkey from the
            //cache which was populated when the ReqContractSigsForSender message arrived
            let contract_spk = redeemscript_to_scriptpubkey(&funding_info.contract_redeemscript);

            if !self.wallet.read()?.does_prevout_match_cached_contract(
                &OutPoint {
                    txid: funding_info.funding_tx.txid(),
                    vout: funding_output_index as u32,
                },
                &contract_spk,
            )? {
                return Err(MakerError::General(
                    "provided contract does not match sender contract tx, rejecting",
                ));
            }
        }

        Ok(check_hashvalues_are_equal(&message)?)
    }

    /// Verify the contract transaction for Sender and return the signatures.
    pub fn verify_and_sign_contract_tx(
        &self,
        message: &ReqContractSigsForSender,
    ) -> Result<Vec<Signature>, MakerError> {
        let mut sigs = Vec::<Signature>::new();
        for txinfo in &message.txs_info {
            if txinfo.senders_contract_tx.input.len() != 1
                || txinfo.senders_contract_tx.output.len() != 1
            {
                return Err(MakerError::General(
                    "invalid number of inputs or outputs in contract transaction",
                ));
            }

            if !self.wallet.read()?.does_prevout_match_cached_contract(
                &txinfo.senders_contract_tx.input[0].previous_output,
                &txinfo.senders_contract_tx.output[0].script_pubkey,
            )? {
                return Err(MakerError::General(
                    "taker attempting multiple contract attack, rejecting",
                ));
            }

            let (tweakable_privkey, tweakable_pubkey) = self.wallet.read()?.get_tweakable_keypair();

            check_multisig_has_pubkey(
                &txinfo.multisig_redeemscript,
                &tweakable_pubkey,
                &txinfo.multisig_nonce,
            )?;

            let secp = Secp256k1::new();

            let mut hashlock_privkey = tweakable_privkey;
            hashlock_privkey.add_assign(txinfo.hashlock_nonce.as_ref())?;

            let hashlock_pubkey = PublicKey {
                compressed: true,
                key: secp256k1::PublicKey::from_secret_key(&secp, &hashlock_privkey),
            };

            crate::protocol::contract::is_contract_out_valid(
                &txinfo.senders_contract_tx.output[0],
                &hashlock_pubkey,
                &txinfo.timelock_pubkey,
                &message.hashvalue,
                &message.locktime,
                &self.config.min_contract_reaction_time,
            )?;

            self.wallet.write()?.cache_prevout_to_contract(
                txinfo.senders_contract_tx.input[0].previous_output,
                txinfo.senders_contract_tx.output[0].script_pubkey.clone(),
            )?;

            let mut multisig_privkey = tweakable_privkey;
            multisig_privkey.add_assign(txinfo.multisig_nonce.as_ref())?;

            let sig = crate::protocol::contract::sign_contract_tx(
                &txinfo.senders_contract_tx,
                &txinfo.multisig_redeemscript,
                txinfo.funding_input_value,
                &multisig_privkey,
            )?;
            sigs.push(sig);
        }
        Ok(sigs)
    }
}

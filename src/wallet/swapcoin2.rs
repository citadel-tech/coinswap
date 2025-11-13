//! SwapCoin 2 structures define ongoing Taproot-based swap operations between two Makers.

use bitcoin::{
    secp256k1::{Scalar, SecretKey, XOnlyPublicKey},
    PublicKey, ScriptBuf, Transaction, Txid,
};

/// Incoming swapcoin for a Taproot swap (active or completed).
#[derive(Debug, Clone, Default)]
pub struct IncomingSwapCoinV2 {
    pub(crate) my_privkey: Option<SecretKey>,
    pub(crate) my_pubkey: Option<PublicKey>,
    pub(crate) other_pubkey: Option<PublicKey>,
    pub(crate) hashlock_script: ScriptBuf,
    pub(crate) timelock_script: ScriptBuf,
    pub(crate) contract_txid: Option<Txid>,
    pub(crate) tap_tweak: Option<Scalar>,
    pub(crate) internal_key: Option<XOnlyPublicKey>,
    pub(crate) spending_tx: Option<Transaction>,
}

impl IncomingSwapCoinV2 {
    /// Returns the Maker’s private key used in this incoming swap, if initialized.
    pub fn privkey(&self) -> Option<&SecretKey> {
        self.my_privkey.as_ref()
    }

    /// Returns the Maker’s public key used for this incoming swap, if available.
    pub fn pubkey(&self) -> Option<&PublicKey> {
        self.my_pubkey.as_ref()
    }

    /// Returns the counterparty's public key included in this incoming swap.
    pub fn other_pubkey(&self) -> Option<&PublicKey> {
        self.other_pubkey.as_ref()
    }

    /// Returns the hashlock script used for the incoming swap.
    pub fn hashlock_script(&self) -> &ScriptBuf {
        &self.hashlock_script
    }

    /// Returns the timelock script for the incoming swap.
    pub fn timelock_script(&self) -> &ScriptBuf {
        &self.timelock_script
    }

    /// Returns the transaction ID of the swap contract transaction, if already broadcasted.
    pub fn contract_txid(&self) -> Option<Txid> {
        self.contract_txid
    }

    /// Returns the Taproot tweak applied
    pub fn tap_tweak(&self) -> Option<&Scalar> {
        self.tap_tweak.as_ref()
    }

    /// Returns the internal X-only public key for the Taproot output
    pub fn internal_key(&self) -> Option<&XOnlyPublicKey> {
        self.internal_key.as_ref()
    }

    /// Returns the spending transaction for this incoming swap, if already created.
    pub fn spending_tx(&self) -> Option<Transaction> {
        self.spending_tx.clone()
    }
}

/// Outgoing swapcoin for a Taproot swap (active or completed).
///
/// Represents the swapcoin that is being sent to another participant.
#[derive(Debug, Clone, Default)]
pub struct OutgoingSwapCoinV2 {
    pub(crate) my_privkey: Option<SecretKey>,
    pub(crate) my_pubkey: Option<PublicKey>,
    pub(crate) other_pubkey: Option<PublicKey>,
    pub(crate) tap_tweak: Option<Scalar>,
    pub(crate) internal_key: Option<XOnlyPublicKey>,
    pub(crate) hashlock_script: ScriptBuf,
    pub(crate) timelock_script: ScriptBuf,
    pub(crate) contract_txid: Option<Txid>,
}

impl OutgoingSwapCoinV2 {
    /// Returns the Maker’s private key used for this outgoing swap
    pub fn privkey(&self) -> Option<&SecretKey> {
        self.my_privkey.as_ref()
    }

    /// Returns the Maker’s public key for this outgoing swap
    pub fn pubkey(&self) -> Option<&PublicKey> {
        self.my_pubkey.as_ref()
    }

    /// Returns the counterparty’s public key for this outgoing swap
    pub fn other_pubkey(&self) -> Option<&PublicKey> {
        self.other_pubkey.as_ref()
    }

    /// Returns the Taproot tweak applied for this outgoing swap
    pub fn tap_tweak(&self) -> Option<&Scalar> {
        self.tap_tweak.as_ref()
    }

    /// Returns the internal X-only public key for the Taproot output
    pub fn internal_key(&self) -> Option<&XOnlyPublicKey> {
        self.internal_key.as_ref()
    }

    /// Returns the hashlock script used for the outgoing contract.
    pub fn hashlock_script(&self) -> &ScriptBuf {
        &self.hashlock_script
    }

    /// Returns the timelock script used for the outgoing contract.
    pub fn timelock_script(&self) -> &ScriptBuf {
        &self.timelock_script
    }

    /// Returns the transaction ID of the swap contract transaction,if broadcasted.
    pub fn contract_txid(&self) -> Option<&Txid> {
        self.contract_txid.as_ref()
    }
}

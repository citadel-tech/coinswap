//! SwapCoin2 structures define ongoing Taproot-based swap operations.

use bitcoin::{
    secp256k1::{Scalar, SecretKey, XOnlyPublicKey},
    PublicKey, ScriptBuf, Transaction, Txid,
};

use crate::wallet::WalletError;

/// Incoming swapcoin for a Taproot swap (active or completed).
///
/// Represents the swapcoin that is being received.
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
    /// Returns the taker or maker's private key used in this incoming swap, if initialized.
    pub fn privkey(&self) -> Result<SecretKey, WalletError> {
        self.my_privkey.ok_or_else(|| {
            WalletError::General("Incoming swapcoin's privkey does not exist".to_string())
        })
    }

    /// Returns the taker or maker's public key used for this incoming swap, if initialized.
    pub fn pubkey(&self) -> Result<PublicKey, WalletError> {
        self.my_pubkey.ok_or_else(|| {
            WalletError::General("Incoming swapcoin's pubkey does not exist".to_string())
        })
    }

    /// Returns the counterparty's public key included in this incoming swap.
    pub fn other_pubkey(&self) -> Result<PublicKey, WalletError> {
        self.other_pubkey.ok_or_else(|| {
            WalletError::General("Incoming swapcoin's other pubkey does not exist".to_string())
        })
    }

    /// Returns the hashlock script used for the incoming swap.
    pub fn hashlock_script(&self) -> &ScriptBuf {
        &self.hashlock_script
    }

    /// Returns the timelock script used for the incoming swap.
    pub fn timelock_script(&self) -> &ScriptBuf {
        &self.timelock_script
    }

    /// Returns the transaction ID of the swap contract transaction, if already broadcasted.
    pub fn contract_txid(&self) -> Result<Txid, WalletError> {
        self.contract_txid.ok_or_else(|| {
            WalletError::General("Incoming swapcoin's contract txid does not exist".to_string())
        })
    }

    /// Returns the Taproot tweak applied
    pub fn tap_tweak(&self) -> Result<Scalar, WalletError> {
        self.tap_tweak.ok_or_else(|| {
            WalletError::General("Incoming swapcoin's tap tweak does not exist".to_string())
        })
    }

    /// Returns the internal X-only public key for the Taproot output
    pub fn internal_key(&self) -> Result<XOnlyPublicKey, WalletError> {
        self.internal_key.ok_or_else(|| {
            WalletError::General("Incoming swapcoin's internal key does not exist".to_string())
        })
    }

    /// Returns the spending transaction for this incoming swap, if already broadcasted.
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
}

impl OutgoingSwapCoinV2 {
    /// Returns the taker or maker's private key used for this outgoing swap
    pub fn privkey(&self) -> Result<SecretKey, WalletError> {
        self.my_privkey.ok_or_else(|| {
            WalletError::General("Outgoing swapcoin's privkey does not exist".to_string())
        })
    }

    /// Returns the taker or maker's public key for this outgoing swap
    pub fn pubkey(&self) -> Result<PublicKey, WalletError> {
        self.my_pubkey.ok_or_else(|| {
            WalletError::General("Outgoing swapcoin's privkey does not exist".to_string())
        })
    }

    /// Returns the counterparty’s public key for this outgoing swap
    pub fn other_pubkey(&self) -> Result<PublicKey, WalletError> {
        self.other_pubkey.ok_or_else(|| {
            WalletError::General("Outgoing swapcoin's other pubkey does not exist".to_string())
        })
    }

    /// Returns the Taproot tweak applied for this outgoing swap
    pub fn tap_tweak(&self) -> Result<Scalar, WalletError> {
        self.tap_tweak.ok_or_else(|| {
            WalletError::General("Outgoing swapcoin's tap tweak does not exist".to_string())
        })
    }

    /// Returns the internal X-only public key for the Taproot output
    pub fn internal_key(&self) -> Result<XOnlyPublicKey, WalletError> {
        self.internal_key.ok_or_else(|| {
            WalletError::General("Outgoing swapcoin's internal key does not exist".to_string())
        })
    }

    /// Returns the hashlock script used for the outgoing contract.
    pub fn hashlock_script(&self) -> &ScriptBuf {
        &self.hashlock_script
    }

    /// Returns the timelock script used for the outgoing contract.
    pub fn timelock_script(&self) -> &ScriptBuf {
        &self.timelock_script
    }
}

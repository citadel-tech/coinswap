//! SwapCoin2 structures define ongoing Taproot-based swap operations.

use bitcoin::{
    secp256k1::{Scalar, SecretKey, XOnlyPublicKey},
    Amount, PublicKey, ScriptBuf, Transaction, Txid,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::convert::TryInto;

mod option_scalar_serde {
    use super::*;

    pub fn serialize<S>(scalar: &Option<Scalar>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match scalar {
            Some(s) => serializer.serialize_some(&s.to_be_bytes()),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Scalar>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let opt: Option<Vec<u8>> = Deserialize::deserialize(deserializer)?;
        opt.map(|bytes| {
            let bytes_array: [u8; 32] = bytes
                .try_into()
                .map_err(|_| serde::de::Error::custom("Invalid scalar byte length"))?;
            Scalar::from_be_bytes(bytes_array)
                .map_err(|e| serde::de::Error::custom(format!("Invalid scalar: {:?}", e)))
        })
        .transpose()
    }
}

use crate::wallet::WalletError;

/// Incoming swapcoin for a Taproot swap (active or completed).
///
/// Represents the swapcoin that is being received.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IncomingSwapCoinV2 {
    pub(crate) my_privkey: Option<SecretKey>,
    pub(crate) my_pubkey: Option<PublicKey>,
    pub(crate) other_pubkey: Option<PublicKey>,
    pub(crate) hashlock_script: ScriptBuf,
    pub(crate) timelock_script: ScriptBuf,
    pub(crate) contract_tx: Transaction,
    pub(crate) contract_txid: Option<Txid>,
    #[serde(with = "option_scalar_serde")]
    pub(crate) tap_tweak: Option<Scalar>,
    pub(crate) internal_key: Option<XOnlyPublicKey>,
    pub(crate) spending_tx: Option<Transaction>,
    pub(crate) hash_preimage: Option<[u8; 32]>,
    pub(crate) other_privkey: Option<SecretKey>,
    pub(crate) hashlock_privkey: SecretKey,
    pub(crate) funding_amount: Amount,
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

    /// Returns the funding amount for this incoming swap.
    pub fn funding_amount(&self) -> Amount {
        self.funding_amount
    }
}

/// Outgoing swapcoin for a Taproot swap (active or completed).
///
/// Represents the swapcoin that is being sent to another participant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutgoingSwapCoinV2 {
    pub(crate) my_privkey: Option<SecretKey>,
    pub(crate) my_pubkey: Option<PublicKey>,
    pub(crate) other_pubkey: Option<PublicKey>,
    #[serde(with = "option_scalar_serde")]
    pub(crate) tap_tweak: Option<Scalar>,
    pub(crate) internal_key: Option<XOnlyPublicKey>,
    pub(crate) hashlock_script: ScriptBuf,
    pub(crate) timelock_script: ScriptBuf,
    pub(crate) contract_tx: Transaction,
    pub(crate) hash_preimage: Option<[u8; 32]>,
    pub(crate) other_privkey: Option<SecretKey>,
    pub(crate) timelock_privkey: SecretKey,
    pub(crate) funding_amount: Amount,
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

    /// Returns the contract transaction for this outgoing swap.
    pub fn contract_tx(&self) -> &Transaction {
        &self.contract_tx
    }

    /// Returns the funding amount for this outgoing swap.
    pub fn funding_amount(&self) -> Amount {
        self.funding_amount
    }

    /// Extracts the timelock value from the timelock script.
    pub fn get_timelock(&self) -> Option<u32> {
        // Parse timelock from OP_CHECKLOCKTIMEVERIFY script
        // Expected format: <locktime> OP_CHECKLOCKTIMEVERIFY OP_DROP <pubkey> OP_CHECKSIG
        let script_bytes = self.timelock_script.as_bytes();

        if script_bytes.len() < 6 {
            return None;
        }

        // First byte should be push opcode for locktime value (1-5 bytes)
        let locktime_len = script_bytes[0] as usize;
        if locktime_len == 0 || locktime_len > 5 || script_bytes.len() < locktime_len + 1 {
            return None;
        }

        // Parse little-endian locktime value
        let mut locktime = 0u32;
        for i in 0..locktime_len {
            locktime |= (script_bytes[1 + i] as u32) << (i * 8);
        }

        Some(locktime)
    }
}

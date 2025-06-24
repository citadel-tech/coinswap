//! SwapCoins are structures defining an ongoing swap operations.
//! They are UTXOs + metadata which are not from the deterministic wallet
//! and made in the process of a CoinSwap.
//!
//! There are three types of SwapCoins:
//! [IncomingSwapCoin]: The contract data defining an **incoming** swap.
//! [OutgoingSwapCoin]: The contract data defining an **outgoing** swap.
//! [WatchOnlySwapCoin]: The contract data defining a **watch-only** swap. This is only applicable for Takers,
//! for monitoring the swaps happening between two Makers.

use bitcoin::{
    ecdsa::Signature,
    secp256k1::{self, Secp256k1, SecretKey},
    sighash::{EcdsaSighashType, SighashCache},
    Amount, PublicKey, Script, ScriptBuf, Transaction, TxIn,
};

use super::WalletError;
use crate::protocol::{
    contract::{
        apply_two_signatures_to_2of2_multisig_spend, create_multisig_redeemscript,
        read_contract_locktime, read_hashlock_pubkey_from_contract, read_hashvalue_from_contract,
        read_pubkeys_from_multisig_redeemscript, read_timelock_pubkey_from_contract,
        sign_contract_tx, verify_contract_tx_sig,
    },
    error::ProtocolError,
    messages::Preimage,
    Hash160,
};

/// Defines an incoming swapcoin, which can either be currently active or successfully completed.
///
/// ### NOTE:
/// The term `Incoming` imply an Incoming Coin from a swap.
/// This can be for either a Taker or a Maker, depending on their position in the swap route.
/// This refers to coins that have been "received" by the party,
/// i.e. the party holds the hash lock side of the contract.
///
/// ### Example:
/// Consider a swap scenario where Alice and Bob are exchanging assets.
/// The coin that Bob receives from Alice is referred to as `Incoming` from Bob's perspective.
/// This designation applies regardless of the swap's status—whether
/// it is still in progress or has been finalized.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub(crate) struct IncomingSwapCoin {
    pub(crate) my_privkey: SecretKey,
    pub(crate) other_pubkey: PublicKey,
    pub(crate) other_privkey: Option<SecretKey>,
    pub(crate) contract_tx: Transaction,
    pub(crate) contract_redeemscript: ScriptBuf,
    pub(crate) hashlock_privkey: SecretKey,
    pub(crate) funding_amount: Amount,
    pub(crate) others_contract_sig: Option<Signature>,
    pub(crate) hash_preimage: Option<Preimage>,
}

/// Describes an outgoing swapcoin, which can either be currently active or successfully completed.
///
/// ### NOTE:
/// The term `Outgoing` imply an Outgoing Coin from a swap.
/// This can be for either a Taker or a Maker, depending on their position in the swap route.
/// This refers to coins that have been "sent" by the party,
/// i.e. the party holds the time lock side of the contract.
///
/// ### Example:
/// In a swap transaction between Alice and Bob,
/// the coin that Alice sends to Bob is referred to as `Outgoing` from Alice's perspective.
/// This terminology reflects the direction of the asset transfer,
/// regardless of whether the swap is still ongoing or has been completed.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub(crate) struct OutgoingSwapCoin {
    pub(crate) my_privkey: SecretKey,
    pub(crate) other_pubkey: PublicKey,
    pub(crate) contract_tx: Transaction,
    pub(crate) contract_redeemscript: ScriptBuf,
    pub(crate) timelock_privkey: SecretKey,
    pub(crate) funding_amount: Amount,
    pub(crate) others_contract_sig: Option<Signature>,
    pub(crate) hash_preimage: Option<Preimage>,
}

/// Represents a watch-only view of a coinswap between two makers.
//like the Incoming/OutgoingSwapCoin structs but no privkey or signature information
//used by the taker to monitor coinswaps between two makers
#[derive(Debug, Clone)]
pub(crate) struct WatchOnlySwapCoin {
    /// Public key of the sender (maker).
    pub(crate) sender_pubkey: PublicKey,
    /// Public key of the receiver (maker).
    pub(crate) receiver_pubkey: PublicKey,
    /// Transaction representing the coinswap contract.
    pub(crate) contract_tx: Transaction,
    /// Redeem script associated with the coinswap contract.
    pub(crate) contract_redeemscript: ScriptBuf,
    /// The funding amount of the coinswap.
    pub(crate) funding_amount: Amount,
}

/// Trait representing common functionality for swap coins.
pub(crate) trait SwapCoin {
    /// Get the multisig redeem script.
    fn get_multisig_redeemscript(&self) -> ScriptBuf;
    /// Get the contract transaction.
    fn get_contract_tx(&self) -> Transaction;
    /// Get the contract redeem script.
    fn get_contract_redeemscript(&self) -> ScriptBuf;
    /// Get the timelock public key.
    fn get_timelock_pubkey(&self) -> Result<PublicKey, WalletError>;
    /// Get the timelock value.
    fn get_timelock(&self) -> Result<u16, WalletError>;
    /// Get the hash value.
    fn get_hashvalue(&self) -> Result<Hash160, WalletError>;
    /// Get the funding amount.
    fn get_funding_amount(&self) -> Amount;
    /// Verify the receiver's signature on the contract transaction.
    fn verify_contract_tx_receiver_sig(&self, sig: &Signature) -> Result<(), WalletError>;
    /// Verify the sender's signature on the contract transaction.
    fn verify_contract_tx_sender_sig(&self, sig: &Signature) -> Result<(), WalletError>;
    /// Apply a private key to the swap coin.
    fn apply_privkey(&mut self, privkey: SecretKey) -> Result<(), ProtocolError>;
}

/// Trait representing swap coin functionality specific to a wallet.
pub(crate) trait WalletSwapCoin: SwapCoin {
    fn get_my_pubkey(&self) -> PublicKey;
    fn get_other_pubkey(&self) -> &PublicKey;
    fn get_fully_signed_contract_tx(&self) -> Result<Transaction, ProtocolError>;
    fn is_hash_preimage_known(&self) -> bool;
}

macro_rules! impl_walletswapcoin {
    ($coin:ident) => {
        impl WalletSwapCoin for $coin {
            fn get_my_pubkey(&self) -> bitcoin::PublicKey {
                let secp = Secp256k1::new();
                PublicKey {
                    compressed: true,
                    inner: secp256k1::PublicKey::from_secret_key(&secp, &self.my_privkey),
                }
            }

            fn get_other_pubkey(&self) -> &PublicKey {
                &self.other_pubkey
            }

            fn get_fully_signed_contract_tx(&self) -> Result<Transaction, ProtocolError> {
                if self.others_contract_sig.is_none() {
                    return Err(ProtocolError::General(
                        "Other's contract signature not known",
                    ));
                }
                let my_pubkey = self.get_my_pubkey();
                let multisig_redeemscript =
                    create_multisig_redeemscript(&my_pubkey, &self.other_pubkey);
                let index = 0;
                let secp = Secp256k1::new();
                let sighash = secp256k1::Message::from_digest_slice(
                    &SighashCache::new(&self.contract_tx)
                        .p2wsh_signature_hash(
                            index,
                            &multisig_redeemscript,
                            self.funding_amount,
                            EcdsaSighashType::All,
                        )
                        .map_err(ProtocolError::Sighash)?[..],
                )
                .map_err(ProtocolError::Secp)?;
                let sig_mine = Signature {
                    signature: secp.sign_ecdsa(&sighash, &self.my_privkey),
                    sighash_type: EcdsaSighashType::All,
                };

                let mut signed_contract_tx = self.contract_tx.clone();
                apply_two_signatures_to_2of2_multisig_spend(
                    &my_pubkey,
                    &self.other_pubkey,
                    &sig_mine,
                    &self
                        .others_contract_sig
                        .expect("others contract sig expected"),
                    &mut signed_contract_tx.input[index],
                    &multisig_redeemscript,
                );
                Ok(signed_contract_tx)
            }

            fn is_hash_preimage_known(&self) -> bool {
                self.hash_preimage.is_some()
            }
        }
    };
}

macro_rules! impl_swapcoin_getters {
    () => {
        //unwrap() here because previously checked that contract_redeemscript is good
        fn get_timelock_pubkey(&self) -> Result<PublicKey, WalletError> {
            Ok(read_timelock_pubkey_from_contract(
                &self.contract_redeemscript,
            )?)
        }

        fn get_timelock(&self) -> Result<u16, WalletError> {
            Ok(read_contract_locktime(&self.contract_redeemscript)?)
        }

        // fn get_hashlock_pubkey(&self) -> Result<PublicKey, WalletError> {
        //     Ok(read_hashlock_pubkey_from_contract(
        //         &self.contract_redeemscript,
        //     )?)
        // }

        fn get_hashvalue(&self) -> Result<Hash160, WalletError> {
            Ok(read_hashvalue_from_contract(&self.contract_redeemscript)?)
        }

        fn get_contract_tx(&self) -> Transaction {
            self.contract_tx.clone()
        }

        fn get_contract_redeemscript(&self) -> ScriptBuf {
            self.contract_redeemscript.clone()
        }

        fn get_funding_amount(&self) -> Amount {
            self.funding_amount
        }
    };
}

impl IncomingSwapCoin {
    pub(crate) fn new(
        my_privkey: SecretKey,
        other_pubkey: PublicKey,
        contract_tx: Transaction,
        contract_redeemscript: ScriptBuf,
        hashlock_privkey: SecretKey,
        funding_amount: Amount,
    ) -> Result<Self, WalletError> {
        let secp = Secp256k1::new();
        let hashlock_pubkey = PublicKey {
            compressed: true,
            inner: secp256k1::PublicKey::from_secret_key(&secp, &hashlock_privkey),
        };
        assert!(hashlock_pubkey == read_hashlock_pubkey_from_contract(&contract_redeemscript)?);
        Ok(Self {
            my_privkey,
            other_pubkey,
            other_privkey: None,
            contract_tx,
            contract_redeemscript,
            hashlock_privkey,
            funding_amount,
            others_contract_sig: None,
            hash_preimage: None,
        })
    }

    pub(crate) fn sign_transaction_input(
        &self,
        index: usize,
        tx: &Transaction,
        input: &mut TxIn,
        redeemscript: &Script,
    ) -> Result<(), ProtocolError> {
        if self.other_privkey.is_none() {
            return Err(ProtocolError::General(
                "Unable to sign: incomplete coinswap for this input",
            ));
        }
        let secp = Secp256k1::new();
        let my_pubkey = self.get_my_pubkey();

        let sighash = secp256k1::Message::from_digest_slice(
            &SighashCache::new(tx)
                .p2wsh_signature_hash(
                    index,
                    redeemscript,
                    self.funding_amount,
                    EcdsaSighashType::All,
                )
                .map_err(ProtocolError::Sighash)?[..],
        )
        .map_err(ProtocolError::Secp)?;

        let sig_mine = Signature {
            signature: secp.sign_ecdsa(&sighash, &self.my_privkey),
            sighash_type: EcdsaSighashType::All,
        };
        let sig_other = Signature {
            signature: secp.sign_ecdsa(
                &sighash,
                &self.other_privkey.expect("other's privatekey expected"),
            ),
            sighash_type: EcdsaSighashType::All,
        };

        apply_two_signatures_to_2of2_multisig_spend(
            &my_pubkey,
            &self.other_pubkey,
            &sig_mine,
            &sig_other,
            input,
            redeemscript,
        );
        Ok(())
    }

    pub(crate) fn sign_hashlocked_transaction_input_given_preimage(
        &self,
        index: usize,
        tx: &Transaction,
        input: &mut TxIn,
        input_value: Amount,
        hash_preimage: &[u8],
    ) -> Result<(), WalletError> {
        let secp = Secp256k1::new();
        let sighash = secp256k1::Message::from_digest_slice(
            &SighashCache::new(tx)
                .p2wsh_signature_hash(
                    index,
                    &self.contract_redeemscript,
                    input_value,
                    EcdsaSighashType::All,
                )
                .map_err(ProtocolError::Sighash)?[..],
        )
        .map_err(ProtocolError::Secp)?;

        let sig_hashlock = secp.sign_ecdsa(&sighash, &self.hashlock_privkey);
        let mut sig_hashlock_bytes = sig_hashlock.serialize_der().to_vec();
        sig_hashlock_bytes.push(EcdsaSighashType::All as u8);
        input.witness.push(sig_hashlock_bytes);
        input.witness.push(hash_preimage);
        input.witness.push(self.contract_redeemscript.to_bytes());
        Ok(())
    }

    pub(crate) fn sign_hashlocked_transaction_input(
        &self,
        index: usize,
        tx: &Transaction,
        input: &mut TxIn,
        input_value: Amount,
    ) -> Result<(), WalletError> {
        if self.hash_preimage.is_none() {
            panic!("invalid state, unable to sign: preimage unknown");
        }
        self.sign_hashlocked_transaction_input_given_preimage(
            index,
            tx,
            input,
            input_value,
            &self.hash_preimage.expect("hash preimage expected"),
        )
    }

    pub(crate) fn verify_contract_tx_sig(&self, sig: &Signature) -> Result<(), WalletError> {
        Ok(verify_contract_tx_sig(
            &self.contract_tx,
            &self.get_multisig_redeemscript(),
            self.funding_amount,
            &self.other_pubkey,
            &sig.signature,
        )?)
    }
}

impl OutgoingSwapCoin {
    pub(crate) fn new(
        my_privkey: SecretKey,
        other_pubkey: PublicKey,
        contract_tx: Transaction,
        contract_redeemscript: ScriptBuf,
        timelock_privkey: SecretKey,
        funding_amount: Amount,
    ) -> Result<Self, WalletError> {
        let secp = Secp256k1::new();
        let timelock_pubkey = PublicKey {
            compressed: true,
            inner: secp256k1::PublicKey::from_secret_key(&secp, &timelock_privkey),
        };
        assert!(timelock_pubkey == read_timelock_pubkey_from_contract(&contract_redeemscript)?);
        Ok(Self {
            my_privkey,
            other_pubkey,
            contract_tx,
            contract_redeemscript,
            timelock_privkey,
            funding_amount,
            others_contract_sig: None,
            hash_preimage: None,
        })
    }

    pub(crate) fn sign_timelocked_transaction_input(
        &self,
        index: usize,
        tx: &Transaction,
        input: &mut TxIn,
        input_value: Amount,
    ) -> Result<(), WalletError> {
        let secp = Secp256k1::new();
        let sighash = secp256k1::Message::from_digest_slice(
            &SighashCache::new(tx)
                .p2wsh_signature_hash(
                    index,
                    &self.contract_redeemscript,
                    input_value,
                    EcdsaSighashType::All,
                )
                .map_err(ProtocolError::Sighash)?[..],
        )
        .map_err(ProtocolError::Secp)?;

        let sig_timelock = secp.sign_ecdsa(&sighash, &self.timelock_privkey);

        let mut sig_timelock_bytes = sig_timelock.serialize_der().to_vec();
        sig_timelock_bytes.push(EcdsaSighashType::All as u8);
        input.witness.push(sig_timelock_bytes);
        input.witness.push(Vec::new());
        input.witness.push(self.contract_redeemscript.to_bytes());
        Ok(())
    }

    //"_with_my_privkey" as opposed to with other_privkey
    pub(crate) fn sign_contract_tx_with_my_privkey(
        &self,
        contract_tx: &Transaction,
    ) -> Result<Signature, WalletError> {
        let multisig_redeemscript = self.get_multisig_redeemscript();
        Ok(sign_contract_tx(
            contract_tx,
            &multisig_redeemscript,
            self.funding_amount,
            &self.my_privkey,
        )?)
    }

    pub(crate) fn verify_contract_tx_sig(&self, sig: &Signature) -> Result<(), WalletError> {
        Ok(verify_contract_tx_sig(
            &self.contract_tx,
            &self.get_multisig_redeemscript(),
            self.funding_amount,
            &self.other_pubkey,
            &sig.signature,
        )?)
    }
}

impl WatchOnlySwapCoin {
    pub(crate) fn new(
        multisig_redeemscript: &ScriptBuf,
        receiver_pubkey: PublicKey,
        contract_tx: Transaction,
        contract_redeemscript: ScriptBuf,
        funding_amount: Amount,
    ) -> Result<WatchOnlySwapCoin, ProtocolError> {
        let (pubkey1, pubkey2) = read_pubkeys_from_multisig_redeemscript(multisig_redeemscript)?;
        if pubkey1 != receiver_pubkey && pubkey2 != receiver_pubkey {
            return Err(ProtocolError::General(
                "Given sender_pubkey not included in redeemscript",
            ));
        }
        let sender_pubkey = if pubkey1 == receiver_pubkey {
            pubkey2
        } else {
            pubkey1
        };
        Ok(WatchOnlySwapCoin {
            sender_pubkey,
            receiver_pubkey,
            contract_tx,
            contract_redeemscript,
            funding_amount,
        })
    }
}

impl_walletswapcoin!(IncomingSwapCoin);
impl_walletswapcoin!(OutgoingSwapCoin);

impl SwapCoin for IncomingSwapCoin {
    impl_swapcoin_getters!();

    fn get_multisig_redeemscript(&self) -> ScriptBuf {
        let secp = Secp256k1::new();
        create_multisig_redeemscript(
            &self.other_pubkey,
            &PublicKey {
                compressed: true,
                inner: secp256k1::PublicKey::from_secret_key(&secp, &self.my_privkey),
            },
        )
    }

    fn verify_contract_tx_receiver_sig(&self, sig: &Signature) -> Result<(), WalletError> {
        self.verify_contract_tx_sig(sig)
    }

    fn verify_contract_tx_sender_sig(&self, sig: &Signature) -> Result<(), WalletError> {
        self.verify_contract_tx_sig(sig)
    }

    fn apply_privkey(&mut self, privkey: SecretKey) -> Result<(), ProtocolError> {
        let secp = Secp256k1::new();
        let pubkey = PublicKey {
            compressed: true,
            inner: secp256k1::PublicKey::from_secret_key(&secp, &privkey),
        };
        if pubkey != self.other_pubkey {
            return Err(ProtocolError::General("Incorrect privkey"));
        }
        self.other_privkey = Some(privkey);
        Ok(())
    }
}

impl SwapCoin for OutgoingSwapCoin {
    impl_swapcoin_getters!();

    fn get_multisig_redeemscript(&self) -> ScriptBuf {
        let secp = Secp256k1::new();
        create_multisig_redeemscript(
            &self.other_pubkey,
            &PublicKey {
                compressed: true,
                inner: secp256k1::PublicKey::from_secret_key(&secp, &self.my_privkey),
            },
        )
    }

    fn verify_contract_tx_receiver_sig(&self, sig: &Signature) -> Result<(), WalletError> {
        self.verify_contract_tx_sig(sig)
    }

    fn verify_contract_tx_sender_sig(&self, sig: &Signature) -> Result<(), WalletError> {
        self.verify_contract_tx_sig(sig)
    }

    fn apply_privkey(&mut self, privkey: SecretKey) -> Result<(), ProtocolError> {
        let secp = Secp256k1::new();
        let pubkey = PublicKey {
            compressed: true,
            inner: secp256k1::PublicKey::from_secret_key(&secp, &privkey),
        };
        if pubkey == self.other_pubkey {
            Ok(())
        } else {
            Err(ProtocolError::General("Incorrect privkey"))
        }
    }
}

impl SwapCoin for WatchOnlySwapCoin {
    impl_swapcoin_getters!();

    fn apply_privkey(&mut self, privkey: SecretKey) -> Result<(), ProtocolError> {
        let secp = Secp256k1::new();
        let pubkey = PublicKey {
            compressed: true,
            inner: secp256k1::PublicKey::from_secret_key(&secp, &privkey),
        };
        if pubkey == self.sender_pubkey || pubkey == self.receiver_pubkey {
            Ok(())
        } else {
            Err(ProtocolError::General("Incorrect privkey"))
        }
    }

    fn get_multisig_redeemscript(&self) -> ScriptBuf {
        create_multisig_redeemscript(&self.sender_pubkey, &self.receiver_pubkey)
    }

    /*
    Potential confusion here:
        verify sender sig uses the receiver_pubkey
        verify receiver sig uses the sender_pubkey
    */
    fn verify_contract_tx_sender_sig(&self, sig: &Signature) -> Result<(), WalletError> {
        Ok(verify_contract_tx_sig(
            &self.contract_tx,
            &self.get_multisig_redeemscript(),
            self.funding_amount,
            &self.receiver_pubkey,
            &sig.signature,
        )?)
    }

    fn verify_contract_tx_receiver_sig(&self, sig: &Signature) -> Result<(), WalletError> {
        Ok(verify_contract_tx_sig(
            &self.contract_tx,
            &self.get_multisig_redeemscript(),
            self.funding_amount,
            &self.sender_pubkey,
            &sig.signature,
        )?)
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    use bitcoin::{
        absolute::LockTime,
        secp256k1::{self, Secp256k1, SecretKey},
        transaction::Version,
        Address, Amount, NetworkKind, OutPoint, PrivateKey, PublicKey, ScriptBuf, Sequence,
        Transaction, TxIn, TxOut, Witness,
    };

    const TEST_CURRENT_HEIGHT: u32 = 100;

    #[test]
    fn test_apply_privkey_watchonly_swapcoin() {
        let secp = Secp256k1::new();

        let privkey_sender = bitcoin::PrivateKey {
            compressed: true,
            network: NetworkKind::Test,
            inner: secp256k1::SecretKey::from_str(
                "0000000000000000000000000000000000000000000000000000000000000001",
            )
            .unwrap(),
        };

        let privkey_receiver = bitcoin::PrivateKey {
            compressed: true,
            network: NetworkKind::Test,
            inner: secp256k1::SecretKey::from_str(
                "0000000000000000000000000000000000000000000000000000000000000002",
            )
            .unwrap(),
        };

        let mut swapcoin = WatchOnlySwapCoin {
            sender_pubkey: PublicKey::from_private_key(&secp, &privkey_sender),
            receiver_pubkey: PublicKey::from_private_key(&secp, &privkey_receiver),
            funding_amount: Amount::from_sat(100),
            contract_tx: Transaction {
                input: vec![],
                output: vec![],
                lock_time: LockTime::ZERO,
                version: Version::TWO,
            },
            contract_redeemscript: ScriptBuf::default(),
        };

        let secret_key_1 =
            SecretKey::from_str("0000000000000000000000000000000000000000000000000000000000000002")
                .unwrap();
        let secret_key_2 =
            SecretKey::from_str("0000000000000000000000000000000000000000000000000000000000000069")
                .unwrap();
        // Test for applying the correct privkey
        assert!(swapcoin.apply_privkey(secret_key_1).is_ok());
        // Test for applying the incorrect privkey
        assert!(swapcoin.apply_privkey(secret_key_2).is_err());
    }

    #[test]
    fn test_apply_privkey_incoming_swapcoin() {
        let secp = Secp256k1::new();
        let other_privkey = bitcoin::PrivateKey {
            compressed: true,
            network: NetworkKind::Test,
            inner: secp256k1::SecretKey::from_str(
                "0000000000000000000000000000000000000000000000000000000000000002",
            )
            .unwrap(),
        };

        let mut incoming_swapcoin = IncomingSwapCoin {
            my_privkey: secp256k1::SecretKey::from_str(
                "0000000000000000000000000000000000000000000000000000000000000003",
            )
            .unwrap(),
            other_privkey: Some(
                secp256k1::SecretKey::from_str(
                    "0000000000000000000000000000000000000000000000000000000000000005",
                )
                .unwrap(),
            ),
            other_pubkey: PublicKey::from_private_key(&secp, &other_privkey),
            contract_tx: Transaction {
                input: vec![],
                output: vec![],
                lock_time: LockTime::ZERO,
                version: Version::TWO,
            },
            contract_redeemscript: ScriptBuf::default(),
            hashlock_privkey: secp256k1::SecretKey::from_str(
                "0000000000000000000000000000000000000000000000000000000000000004",
            )
            .unwrap(),
            funding_amount: Amount::ZERO,
            others_contract_sig: None,
            hash_preimage: None,
        };

        let secret_key_1 =
            SecretKey::from_str("0000000000000000000000000000000000000000000000000000000000000002")
                .unwrap();
        let secret_key_2 =
            SecretKey::from_str("0000000000000000000000000000000000000000000000000000000000000069")
                .unwrap();
        // Test for applying the correct privkey
        assert!(incoming_swapcoin.apply_privkey(secret_key_1).is_ok());
        // Test for applying the incorrect privkey
        assert!(incoming_swapcoin.apply_privkey(secret_key_2).is_err());
        // Test get_other_pubkey
        let other_pubkey_from_method = incoming_swapcoin.get_other_pubkey();
        assert_eq!(other_pubkey_from_method, &incoming_swapcoin.other_pubkey);
        // Test is_hash_preimage_known for empty hash_preimage
        assert!(!incoming_swapcoin.is_hash_preimage_known());
    }

    #[test]

    fn test_apply_privkey_outgoing_swapcoin() {
        let secp = Secp256k1::new();
        let other_privkey = bitcoin::PrivateKey {
            compressed: true,
            network: NetworkKind::Test,
            inner: secp256k1::SecretKey::from_str(
                "0000000000000000000000000000000000000000000000000000000000000001",
            )
            .unwrap(),
        };
        let mut outgoing_swapcoin = OutgoingSwapCoin {
            my_privkey: secp256k1::SecretKey::from_str(
                "0000000000000000000000000000000000000000000000000000000000000002",
            )
            .unwrap(),
            other_pubkey: PublicKey::from_private_key(&secp, &other_privkey),
            contract_tx: Transaction {
                input: vec![],
                output: vec![],
                lock_time: LockTime::ZERO,
                version: Version::TWO,
            },
            contract_redeemscript: ScriptBuf::default(),
            timelock_privkey: secp256k1::SecretKey::from_str(
                "0000000000000000000000000000000000000000000000000000000000000003",
            )
            .unwrap(),
            funding_amount: Amount::ZERO,
            others_contract_sig: None,
            hash_preimage: None,
        };
        let secret_key_1 =
            SecretKey::from_str("0000000000000000000000000000000000000000000000000000000000000001")
                .unwrap();
        let secret_key_2 =
            SecretKey::from_str("0000000000000000000000000000000000000000000000000000000000000069")
                .unwrap();

        // Test for applying the correct privkey
        assert!(outgoing_swapcoin.apply_privkey(secret_key_1).is_ok());
        // Test for applying the incorrect privkey
        assert!(outgoing_swapcoin.apply_privkey(secret_key_2).is_err());
        // Test get_other_pubkey
        assert_eq!(
            outgoing_swapcoin.get_other_pubkey(),
            &outgoing_swapcoin.other_pubkey
        );
        // Test is_hash_preimage_known
        assert!(!outgoing_swapcoin.is_hash_preimage_known());
    }

    #[test]
    fn test_sign_transaction_input_fail() {
        let secp = Secp256k1::new();
        let other_privkey = bitcoin::PrivateKey {
            compressed: true,
            network: NetworkKind::Test,
            inner: secp256k1::SecretKey::from_str(
                "0000000000000000000000000000000000000000000000000000000000000002",
            )
            .unwrap(),
        };
        let index: usize = 10;
        let mut input = TxIn::default();
        let tx = Transaction {
            input: vec![input.clone()],
            output: vec![],
            lock_time: LockTime::from_height(TEST_CURRENT_HEIGHT).unwrap(),
            version: Version::TWO,
        };

        let contract_redeemscript = ScriptBuf::default(); // Example redeem script

        let incoming_swapcoin = IncomingSwapCoin {
            my_privkey: secp256k1::SecretKey::from_str(
                "0000000000000000000000000000000000000000000000000000000000000003",
            )
            .unwrap(),
            other_privkey: Some(
                secp256k1::SecretKey::from_str(
                    "0000000000000000000000000000000000000000000000000000000000000005",
                )
                .unwrap(),
            ),
            other_pubkey: PublicKey::from_private_key(&secp, &other_privkey),
            contract_tx: Transaction {
                input: vec![],
                output: vec![],
                lock_time: LockTime::ZERO,
                version: Version::TWO,
            },
            contract_redeemscript: ScriptBuf::default(),
            hashlock_privkey: secp256k1::SecretKey::from_str(
                "0000000000000000000000000000000000000000000000000000000000000004",
            )
            .unwrap(),
            funding_amount: Amount::from_sat(100_000),
            others_contract_sig: None,
            hash_preimage: None,
        };
        // Intentionally failing to sign with incomplete swapcoin
        assert!(incoming_swapcoin
            .sign_transaction_input(index, &tx, &mut input, &contract_redeemscript)
            .is_err());
        let sign = bitcoin::ecdsa::Signature {
            signature: secp256k1::ecdsa::Signature::from_compact(&[0; 64]).unwrap(),
            sighash_type: bitcoin::sighash::EcdsaSighashType::All,
        };
        // Intentionally failing to verify with incomplete swapcoin
        assert!(incoming_swapcoin
            .verify_contract_tx_sender_sig(&sign)
            .is_err());
    }

    #[test]

    fn test_create_hashlock_spend_without_preimage() {
        let secp = Secp256k1::new();
        let other_privkey = PrivateKey {
            compressed: true,
            network: NetworkKind::Test,
            inner: secp256k1::SecretKey::from_str(
                "0000000000000000000000000000000000000000000000000000000000000001",
            )
            .unwrap(),
        };
        let input = TxIn::default();
        let output = TxOut::NULL;
        let incoming_swapcoin = IncomingSwapCoin {
            my_privkey: secp256k1::SecretKey::from_str(
                "0000000000000000000000000000000000000000000000000000000000000003",
            )
            .unwrap(),
            other_privkey: Some(
                secp256k1::SecretKey::from_str(
                    "0000000000000000000000000000000000000000000000000000000000000005",
                )
                .unwrap(),
            ),
            other_pubkey: PublicKey::from_private_key(&secp, &other_privkey),
            contract_tx: Transaction {
                input: vec![input.clone()],
                output: vec![output.clone()],
                lock_time: LockTime::ZERO,
                version: Version::TWO,
            },
            contract_redeemscript: ScriptBuf::default(),
            hashlock_privkey: secp256k1::SecretKey::from_str(
                "0000000000000000000000000000000000000000000000000000000000000004",
            )
            .unwrap(),
            funding_amount: Amount::from_sat(100_000),
            others_contract_sig: None,
            hash_preimage: Some(Preimage::from([0; 32])),
        };
        let destination_address: Address = Address::from_str("32iVBEu4dxkUQk9dJbZUiBiQdmypcEyJRf")
            .unwrap()
            .require_network(bitcoin::Network::Bitcoin)
            .unwrap();

        let miner_fee = 136 * 10; //126 vbytes x 10 sat/vb, size calculated using testmempoolaccept
        let mut tx = Transaction {
            input: vec![TxIn {
                previous_output: OutPoint {
                    txid: incoming_swapcoin.contract_tx.compute_txid(),
                    vout: 0, //contract_tx is one-input-one-output
                },
                sequence: Sequence(1), //hashlock spends must have 1 because of the `OP_CSV 1`
                witness: Witness::new(),
                script_sig: ScriptBuf::new(),
            }],
            output: vec![TxOut {
                script_pubkey: destination_address.script_pubkey(),
                value: Amount::from_sat(
                    incoming_swapcoin.contract_tx.output[0].value.to_sat() - miner_fee,
                ),
            }],
            lock_time: LockTime::from_height(TEST_CURRENT_HEIGHT).unwrap(),
            version: Version::TWO,
        };
        let index = 0;
        let preimage = Vec::new();
        incoming_swapcoin
            .sign_hashlocked_transaction_input_given_preimage(
                index,
                &tx.clone(),
                &mut tx.input[0],
                incoming_swapcoin.contract_tx.output[0].value,
                &preimage,
            )
            .unwrap();
        // If the tx is successful, check some field like:
        assert!(tx.input[0].witness.len() == 3);
    }

    #[test]
    fn test_sign_hashlocked_transaction_input() {
        let secp = Secp256k1::new();
        let other_privkey = PrivateKey {
            compressed: true,
            network: NetworkKind::Test,
            inner: secp256k1::SecretKey::from_str(
                "0000000000000000000000000000000000000000000000000000000000000001",
            )
            .unwrap(),
        };
        let mut input = TxIn::default();
        let output = TxOut::NULL;
        let incoming_swapcoin = IncomingSwapCoin {
            my_privkey: secp256k1::SecretKey::from_str(
                "0000000000000000000000000000000000000000000000000000000000000003",
            )
            .unwrap(),
            other_privkey: Some(
                secp256k1::SecretKey::from_str(
                    "0000000000000000000000000000000000000000000000000000000000000005",
                )
                .unwrap(),
            ),
            other_pubkey: PublicKey::from_private_key(&secp, &other_privkey),
            contract_tx: Transaction {
                input: vec![input.clone()],
                output: vec![output.clone()],
                lock_time: LockTime::ZERO,
                version: Version::TWO,
            },
            contract_redeemscript: ScriptBuf::default(),
            hashlock_privkey: secp256k1::SecretKey::from_str(
                "0000000000000000000000000000000000000000000000000000000000000004",
            )
            .unwrap(),
            funding_amount: Amount::from_sat(100_000),
            others_contract_sig: None,
            hash_preimage: Some(Preimage::from([0; 32])),
        };
        let destination_address: Address = Address::from_str("32iVBEu4dxkUQk9dJbZUiBiQdmypcEyJRf")
            .unwrap()
            .require_network(bitcoin::Network::Bitcoin)
            .unwrap();

        let miner_fee = 136 * 10;
        let mut tx = Transaction {
            input: vec![TxIn {
                previous_output: OutPoint {
                    txid: incoming_swapcoin.contract_tx.compute_txid(),
                    vout: 0, //contract_tx is one-input-one-output
                },
                sequence: Sequence(1), //hashlock spends must have 1 because of the `OP_CSV 1`
                witness: Witness::new(),
                script_sig: ScriptBuf::new(),
            }],
            output: vec![TxOut {
                script_pubkey: destination_address.script_pubkey(),
                value: Amount::from_sat(
                    incoming_swapcoin.contract_tx.output[0].value.to_sat() - miner_fee,
                ),
            }],
            lock_time: LockTime::from_height(TEST_CURRENT_HEIGHT).unwrap(),
            version: Version::TWO,
        };
        let index = 0;
        let input_value = Amount::from_sat(100);
        let preimage = Vec::new();
        incoming_swapcoin
            .sign_hashlocked_transaction_input_given_preimage(
                index,
                &tx.clone(),
                &mut tx.input[0],
                incoming_swapcoin.contract_tx.output[0].value,
                &preimage,
            )
            .unwrap();
        // Check if the hashlocked transaction input is successful
        let final_return = incoming_swapcoin.sign_hashlocked_transaction_input(
            index,
            &tx,
            &mut input,
            input_value,
        );
        assert!(final_return.is_ok());
    }
}

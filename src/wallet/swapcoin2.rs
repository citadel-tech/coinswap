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
    pub(crate) my_sec_nonce: SecretNonce,
    pub(crate) other_nonce: Option<PublicNonce>,
    pub(crate) contract_tx: Option<Transaction>,
    pub(crate) hashlock_script: ScriptBuf,
    pub(crate) timelock: Locktime,
    pub(crate) hash_preimage: Option<Preimage>,
    pub(crate) contract_amount: Amount,
    pub(crate) other_partial_sig: Option<Signature>,
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
    pub(crate) my_sec_nonce: SecretNonce,
    pub(crate) other_nonce: Option<PublicNonce>,
    pub(crate) contract_tx: Option<Transaction>,
    pub(crate) timelock: Locktime,
    pub(crate) hashlock_script: ScriptBuf,
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
    pub(crate) hashlock_script: ScriptBuf,
    /// The funding amount of the coinswap.
    pub(crate) contract_amount: Amount,
    pub(crate) sender_partial_sig: Option<Signature>,
    pub(crate) sender_pub_nonce: Option<PublicNonce>,
    pub(crate) receiver_pub_nonce: Option<PublicNonce>,
}

///[TAPROOT] TODO: Replace the below functions with the new Taproot functions

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
                        .expect("others contract sig expeccted"),
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
                "given sender_pubkey not included in redeemscript",
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
            return Err(ProtocolError::General("not correct privkey"));
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
            Err(ProtocolError::General("not correct privkey"))
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
            Err(ProtocolError::General("not correct privkey"))
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
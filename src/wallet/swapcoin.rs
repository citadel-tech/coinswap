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
    absolute::LockTime,
    ecdsa::Signature,
    secp256k1::{self, Secp256k1, SecretKey},
    sighash::{EcdsaSighashType, SighashCache},
    Address, OutPoint, PublicKey, Script, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Witness,
};

use crate::protocol::{
    contract::{
        apply_two_signatures_to_2of2_multisig_spend, create_multisig_redeemscript,
        read_contract_locktime, read_hashlock_pubkey_from_contract, read_hashvalue_from_contract,
        read_pubkeys_from_multisig_redeemscript, read_timelock_pubkey_from_contract,
        sign_contract_tx, verify_contract_tx_sig,
    },
    error::ContractError,
    messages::Preimage,
    Hash160,
};

use super::WalletError;

/// Represents an incoming swapcoin.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct IncomingSwapCoin {
    pub my_privkey: SecretKey,
    pub other_pubkey: PublicKey,
    pub other_privkey: Option<SecretKey>,
    pub contract_tx: Transaction,
    pub contract_redeemscript: ScriptBuf,
    pub hashlock_privkey: SecretKey,
    pub funding_amount: u64,
    pub others_contract_sig: Option<Signature>,
    pub hash_preimage: Option<Preimage>,
}

/// Represents an outgoing swapcoin.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct OutgoingSwapCoin {
    pub my_privkey: SecretKey,
    pub other_pubkey: PublicKey,
    pub contract_tx: Transaction,
    pub contract_redeemscript: ScriptBuf,
    pub timelock_privkey: SecretKey,
    pub funding_amount: u64,
    pub others_contract_sig: Option<Signature>,
    pub hash_preimage: Option<Preimage>,
}

/// Represents a watch-only view of a coinswap between two makers.
//like the Incoming/OutgoingSwapCoin structs but no privkey or signature information
//used by the taker to monitor coinswaps between two makers
#[derive(Debug, Clone)]
pub struct WatchOnlySwapCoin {
    /// Public key of the sender (maker).
    pub sender_pubkey: PublicKey,
    /// Public key of the receiver (maker).
    pub receiver_pubkey: PublicKey,
    /// Transaction representing the coinswap contract.
    pub contract_tx: Transaction,
    /// Redeem script associated with the coinswap contract.
    pub contract_redeemscript: ScriptBuf,
    /// The funding amount of the coinswap.
    pub funding_amount: u64,
}

/// Trait representing common functionality for swap coins.
pub trait SwapCoin {
    /// Get the multisig redeem script.
    fn get_multisig_redeemscript(&self) -> ScriptBuf;
    /// Get the contract transaction.
    fn get_contract_tx(&self) -> Transaction;
    /// Get the contract redeem script.
    fn get_contract_redeemscript(&self) -> ScriptBuf;
    /// Get the timelock public key.
    fn get_timelock_pubkey(&self) -> PublicKey;
    /// Get the timelock value.
    fn get_timelock(&self) -> u16;
    /// Get the hashlock public key.
    fn get_hashlock_pubkey(&self) -> PublicKey;
    /// Get the hash value.
    fn get_hashvalue(&self) -> Hash160;
    /// Get the funding amount.
    fn get_funding_amount(&self) -> u64;
    /// Verify the receiver's signature on the contract transaction.
    fn verify_contract_tx_receiver_sig(&self, sig: &Signature) -> Result<(), WalletError>;
    /// Verify the sender's signature on the contract transaction.
    fn verify_contract_tx_sender_sig(&self, sig: &Signature) -> Result<(), WalletError>;
    /// Apply a private key to the swap coin.
    fn apply_privkey(&mut self, privkey: SecretKey) -> Result<(), WalletError>;
}

/// Trait representing swap coin functionality specific to a wallet.
pub trait WalletSwapCoin: SwapCoin {
    fn get_my_pubkey(&self) -> PublicKey;
    fn get_other_pubkey(&self) -> &PublicKey;
    fn get_fully_signed_contract_tx(&self) -> Result<Transaction, WalletError>;
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

            fn get_fully_signed_contract_tx(&self) -> Result<Transaction, WalletError> {
                if self.others_contract_sig.is_none() {
                    return Err(WalletError::Protocol(
                        "Other's contract signature not known".to_string(),
                    ));
                }
                let my_pubkey = self.get_my_pubkey();
                let multisig_redeemscript =
                    create_multisig_redeemscript(&my_pubkey, &self.other_pubkey);
                let index = 0;
                let secp = Secp256k1::new();
                let sighash = secp256k1::Message::from_slice(
                    &SighashCache::new(&self.contract_tx)
                        .segwit_signature_hash(
                            index,
                            &multisig_redeemscript,
                            self.funding_amount,
                            EcdsaSighashType::All,
                        )
                        .map_err(ContractError::Sighash)?[..],
                )
                .map_err(ContractError::Secp)?;
                let sig_mine = Signature {
                    sig: secp.sign_ecdsa(&sighash, &self.my_privkey),
                    hash_ty: EcdsaSighashType::All,
                };

                let mut signed_contract_tx = self.contract_tx.clone();
                apply_two_signatures_to_2of2_multisig_spend(
                    &my_pubkey,
                    &self.other_pubkey,
                    &sig_mine,
                    &self.others_contract_sig.unwrap(),
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
        fn get_timelock_pubkey(&self) -> PublicKey {
            read_timelock_pubkey_from_contract(&self.contract_redeemscript).unwrap()
        }

        fn get_timelock(&self) -> u16 {
            read_contract_locktime(&self.contract_redeemscript).unwrap()
        }

        fn get_hashlock_pubkey(&self) -> PublicKey {
            read_hashlock_pubkey_from_contract(&self.contract_redeemscript).unwrap()
        }

        fn get_hashvalue(&self) -> Hash160 {
            read_hashvalue_from_contract(&self.contract_redeemscript).unwrap()
        }

        fn get_contract_tx(&self) -> Transaction {
            self.contract_tx.clone()
        }

        fn get_contract_redeemscript(&self) -> ScriptBuf {
            self.contract_redeemscript.clone()
        }

        fn get_funding_amount(&self) -> u64 {
            self.funding_amount
        }
    };
}

impl IncomingSwapCoin {
    pub fn new(
        my_privkey: SecretKey,
        other_pubkey: PublicKey,
        contract_tx: Transaction,
        contract_redeemscript: ScriptBuf,
        hashlock_privkey: SecretKey,
        funding_amount: u64,
    ) -> Self {
        let secp = Secp256k1::new();
        let hashlock_pubkey = PublicKey {
            compressed: true,
            inner: secp256k1::PublicKey::from_secret_key(&secp, &hashlock_privkey),
        };
        assert!(
            hashlock_pubkey == read_hashlock_pubkey_from_contract(&contract_redeemscript).unwrap()
        );
        Self {
            my_privkey,
            other_pubkey,
            other_privkey: None,
            contract_tx,
            contract_redeemscript,
            hashlock_privkey,
            funding_amount,
            others_contract_sig: None,
            hash_preimage: None,
        }
    }

    pub fn sign_transaction_input(
        &self,
        index: usize,
        tx: &Transaction,
        input: &mut TxIn,
        redeemscript: &Script,
    ) -> Result<(), WalletError> {
        if self.other_privkey.is_none() {
            return Err(WalletError::Protocol(
                "Unable to sign: incomplete coinswap for this input".to_string(),
            ));
        }
        let secp = Secp256k1::new();
        let my_pubkey = self.get_my_pubkey();

        let sighash = secp256k1::Message::from_slice(
            &SighashCache::new(tx)
                .segwit_signature_hash(
                    index,
                    redeemscript,
                    self.funding_amount,
                    EcdsaSighashType::All,
                )
                .map_err(ContractError::Sighash)?[..],
        )
        .map_err(ContractError::Secp)?;

        let sig_mine = Signature {
            sig: secp.sign_ecdsa(&sighash, &self.my_privkey),
            hash_ty: EcdsaSighashType::All,
        };
        let sig_other = Signature {
            sig: secp.sign_ecdsa(&sighash, &self.other_privkey.unwrap()),
            hash_ty: EcdsaSighashType::All,
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

    pub fn sign_hashlocked_transaction_input_given_preimage(
        &self,
        index: usize,
        tx: &Transaction,
        input: &mut TxIn,
        input_value: u64,
        hash_preimage: &[u8],
    ) -> Result<(), WalletError> {
        let secp = Secp256k1::new();
        let sighash = secp256k1::Message::from_slice(
            &SighashCache::new(tx)
                .segwit_signature_hash(
                    index,
                    &self.contract_redeemscript,
                    input_value,
                    EcdsaSighashType::All,
                )
                .map_err(ContractError::Sighash)?[..],
        )
        .map_err(ContractError::Secp)?;

        let sig_hashlock = secp.sign_ecdsa(&sighash, &self.hashlock_privkey);
        let mut sig_hashlock_bytes = sig_hashlock.serialize_der().to_vec();
        sig_hashlock_bytes.push(EcdsaSighashType::All as u8);
        input.witness.push(sig_hashlock_bytes);
        input.witness.push(hash_preimage);
        input.witness.push(self.contract_redeemscript.to_bytes());
        Ok(())
    }

    pub fn sign_hashlocked_transaction_input(
        &self,
        index: usize,
        tx: &Transaction,
        input: &mut TxIn,
        input_value: u64,
    ) -> Result<(), WalletError> {
        if self.hash_preimage.is_none() {
            panic!("invalid state, unable to sign: preimage unknown");
        }
        self.sign_hashlocked_transaction_input_given_preimage(
            index,
            tx,
            input,
            input_value,
            &self.hash_preimage.unwrap(),
        )
    }

    pub fn create_hashlock_spend_without_preimage(
        &self,
        destination_address: &Address,
    ) -> Transaction {
        let miner_fee = 136 * 10; //126 vbytes x 10 sat/vb, size calculated using testmempoolaccept
        let mut tx = Transaction {
            input: vec![TxIn {
                previous_output: OutPoint {
                    txid: self.contract_tx.txid(),
                    vout: 0, //contract_tx is one-input-one-output
                },
                sequence: Sequence(1), //hashlock spends must have 1 because of the `OP_CSV 1`
                witness: Witness::new(),
                script_sig: ScriptBuf::new(),
            }],
            output: vec![TxOut {
                script_pubkey: destination_address.script_pubkey(),
                value: self.contract_tx.output[0].value - miner_fee,
            }],
            lock_time: LockTime::ZERO,
            version: 2,
        };
        let index = 0;
        let preimage = Vec::new();
        self.sign_hashlocked_transaction_input_given_preimage(
            index,
            &tx.clone(),
            &mut tx.input[0],
            self.contract_tx.output[0].value,
            &preimage,
        )
        .unwrap();
        tx
    }

    pub fn verify_contract_tx_sig(&self, sig: &Signature) -> Result<(), WalletError> {
        Ok(verify_contract_tx_sig(
            &self.contract_tx,
            &self.get_multisig_redeemscript(),
            self.funding_amount,
            &self.other_pubkey,
            &sig.sig,
        )?)
    }
}

impl OutgoingSwapCoin {
    pub fn new(
        my_privkey: SecretKey,
        other_pubkey: PublicKey,
        contract_tx: Transaction,
        contract_redeemscript: ScriptBuf,
        timelock_privkey: SecretKey,
        funding_amount: u64,
    ) -> Self {
        let secp = Secp256k1::new();
        let timelock_pubkey = PublicKey {
            compressed: true,
            inner: secp256k1::PublicKey::from_secret_key(&secp, &timelock_privkey),
        };
        assert!(
            timelock_pubkey == read_timelock_pubkey_from_contract(&contract_redeemscript).unwrap()
        );
        Self {
            my_privkey,
            other_pubkey,
            contract_tx,
            contract_redeemscript,
            timelock_privkey,
            funding_amount,
            others_contract_sig: None,
            hash_preimage: None,
        }
    }

    pub fn sign_timelocked_transaction_input(
        &self,
        index: usize,
        tx: &Transaction,
        input: &mut TxIn,
        input_value: u64,
    ) -> Result<(), WalletError> {
        let secp = Secp256k1::new();
        let sighash = secp256k1::Message::from_slice(
            &SighashCache::new(tx)
                .segwit_signature_hash(
                    index,
                    &self.contract_redeemscript,
                    input_value,
                    EcdsaSighashType::All,
                )
                .map_err(ContractError::Sighash)?[..],
        )
        .map_err(ContractError::Secp)?;

        let sig_timelock = secp.sign_ecdsa(&sighash, &self.timelock_privkey);

        let mut sig_timelock_bytes = sig_timelock.serialize_der().to_vec();
        sig_timelock_bytes.push(EcdsaSighashType::All as u8);
        input.witness.push(sig_timelock_bytes);
        input.witness.push(Vec::new());
        input.witness.push(self.contract_redeemscript.to_bytes());
        Ok(())
    }

    pub fn create_timelock_spend(&self, destination_address: &Address) -> Transaction {
        let miner_fee = 128 * 2; //128 vbytes x 2 sat/vb, size calculated using testmempoolaccept
        let mut tx = Transaction {
            input: vec![TxIn {
                previous_output: OutPoint {
                    txid: self.contract_tx.txid(),
                    vout: 0, //contract_tx is one-input-one-output
                },
                sequence: Sequence(self.get_timelock() as u32),
                witness: Witness::new(),
                script_sig: ScriptBuf::new(),
            }],
            output: vec![TxOut {
                script_pubkey: destination_address.script_pubkey(),
                value: self.contract_tx.output[0].value - miner_fee,
            }],
            lock_time: LockTime::ZERO,
            version: 2,
        };
        let index = 0;
        self.sign_timelocked_transaction_input(
            index,
            &tx.clone(),
            &mut tx.input[0],
            self.contract_tx.output[0].value,
        )
        .unwrap();
        tx
    }

    //"_with_my_privkey" as opposed to with other_privkey
    pub fn sign_contract_tx_with_my_privkey(
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

    pub fn verify_contract_tx_sig(&self, sig: &Signature) -> Result<(), WalletError> {
        Ok(verify_contract_tx_sig(
            &self.contract_tx,
            &self.get_multisig_redeemscript(),
            self.funding_amount,
            &self.other_pubkey,
            &sig.sig,
        )?)
    }
}

impl WatchOnlySwapCoin {
    pub fn new(
        multisig_redeemscript: &ScriptBuf,
        receiver_pubkey: PublicKey,
        contract_tx: Transaction,
        contract_redeemscript: ScriptBuf,
        funding_amount: u64,
    ) -> Result<WatchOnlySwapCoin, WalletError> {
        let (pubkey1, pubkey2) = read_pubkeys_from_multisig_redeemscript(multisig_redeemscript)?;
        if pubkey1 != receiver_pubkey && pubkey2 != receiver_pubkey {
            return Err(WalletError::Protocol(
                "given sender_pubkey not included in redeemscript".to_string(),
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

    fn apply_privkey(&mut self, privkey: SecretKey) -> Result<(), WalletError> {
        let secp = Secp256k1::new();
        let pubkey = PublicKey {
            compressed: true,
            inner: secp256k1::PublicKey::from_secret_key(&secp, &privkey),
        };
        if pubkey != self.other_pubkey {
            return Err(WalletError::Protocol("not correct privkey".to_string()));
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

    fn apply_privkey(&mut self, privkey: SecretKey) -> Result<(), WalletError> {
        let secp = Secp256k1::new();
        let pubkey = PublicKey {
            compressed: true,
            inner: secp256k1::PublicKey::from_secret_key(&secp, &privkey),
        };
        if pubkey == self.other_pubkey {
            Ok(())
        } else {
            Err(WalletError::Protocol("not correct privkey".to_string()))
        }
    }
}

impl SwapCoin for WatchOnlySwapCoin {
    impl_swapcoin_getters!();

    fn apply_privkey(&mut self, privkey: SecretKey) -> Result<(), WalletError> {
        let secp = Secp256k1::new();
        let pubkey = PublicKey {
            compressed: true,
            inner: secp256k1::PublicKey::from_secret_key(&secp, &privkey),
        };
        if pubkey == self.sender_pubkey || pubkey == self.receiver_pubkey {
            Ok(())
        } else {
            Err(WalletError::Protocol("not correct privkey".to_string()))
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
            &sig.sig,
        )?)
    }

    fn verify_contract_tx_receiver_sig(&self, sig: &Signature) -> Result<(), WalletError> {
        Ok(verify_contract_tx_sig(
            &self.contract_tx,
            &self.get_multisig_redeemscript(),
            self.funding_amount,
            &self.sender_pubkey,
            &sig.sig,
        )?)
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;
    use bitcoin::PrivateKey;

    #[test]
    fn test_apply_privkey_watchonly_swapcoin() {
        let secp = Secp256k1::new();

        let privkey_sender = bitcoin::PrivateKey {
            compressed: true,
            network: bitcoin::Network::Testnet,
            inner: secp256k1::SecretKey::from_str(
                "0000000000000000000000000000000000000000000000000000000000000001",
            )
            .unwrap(),
        };

        let privkey_receiver = bitcoin::PrivateKey {
            compressed: true,
            network: bitcoin::Network::Testnet,
            inner: secp256k1::SecretKey::from_str(
                "0000000000000000000000000000000000000000000000000000000000000002",
            )
            .unwrap(),
        };

        let mut swapcoin = WatchOnlySwapCoin {
            sender_pubkey: PublicKey::from_private_key(&secp, &privkey_sender),
            receiver_pubkey: PublicKey::from_private_key(&secp, &privkey_receiver),
            funding_amount: 100,
            contract_tx: Transaction {
                input: vec![],
                output: vec![],
                lock_time: LockTime::ZERO,
                version: 2,
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
            network: bitcoin::Network::Testnet,
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
                version: 2,
            },
            contract_redeemscript: ScriptBuf::default(),
            hashlock_privkey: secp256k1::SecretKey::from_str(
                "0000000000000000000000000000000000000000000000000000000000000004",
            )
            .unwrap(),
            funding_amount: 0,
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
            network: bitcoin::Network::Testnet,
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
                version: 2,
            },
            contract_redeemscript: ScriptBuf::default(),
            timelock_privkey: secp256k1::SecretKey::from_str(
                "0000000000000000000000000000000000000000000000000000000000000003",
            )
            .unwrap(),
            funding_amount: 0,
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
            network: bitcoin::Network::Testnet,
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
            lock_time: LockTime::ZERO,
            version: 2,
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
                version: 2,
            },
            contract_redeemscript: ScriptBuf::default(),
            hashlock_privkey: secp256k1::SecretKey::from_str(
                "0000000000000000000000000000000000000000000000000000000000000004",
            )
            .unwrap(),
            funding_amount: 100_000,
            others_contract_sig: None,
            hash_preimage: None,
        };
        // Intentionally failing to sign with incomplete swapcoin
        assert!(incoming_swapcoin
            .sign_transaction_input(index, &tx, &mut input, &contract_redeemscript,)
            .is_err());
        let sign = bitcoin::ecdsa::Signature {
            sig: secp256k1::ecdsa::Signature::from_compact(&[0; 64]).unwrap(),
            hash_ty: bitcoin::sighash::EcdsaSighashType::All,
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
            network: bitcoin::Network::Bitcoin,
            inner: secp256k1::SecretKey::from_str(
                "0000000000000000000000000000000000000000000000000000000000000001",
            )
            .unwrap(),
        };
        let input = TxIn::default();
        let output = TxOut::default();
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
                version: 2,
            },
            contract_redeemscript: ScriptBuf::default(),
            hashlock_privkey: secp256k1::SecretKey::from_str(
                "0000000000000000000000000000000000000000000000000000000000000004",
            )
            .unwrap(),
            funding_amount: 100_000,
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
                    txid: incoming_swapcoin.contract_tx.txid(),
                    vout: 0, //contract_tx is one-input-one-output
                },
                sequence: Sequence(1), //hashlock spends must have 1 because of the `OP_CSV 1`
                witness: Witness::new(),
                script_sig: ScriptBuf::new(),
            }],
            output: vec![TxOut {
                script_pubkey: destination_address.script_pubkey(),
                value: incoming_swapcoin.contract_tx.output[0].value - miner_fee,
            }],
            lock_time: LockTime::ZERO,
            version: 2,
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
        // If the tx is succesful, check some field like:
        assert!(tx.input[0].witness.len() == 3);
    }

    #[test]
    fn test_sign_hashlocked_transaction_input() {
        let secp = Secp256k1::new();
        let other_privkey = PrivateKey {
            compressed: true,
            network: bitcoin::Network::Bitcoin,
            inner: secp256k1::SecretKey::from_str(
                "0000000000000000000000000000000000000000000000000000000000000001",
            )
            .unwrap(),
        };
        let mut input = TxIn::default();
        let output = TxOut::default();
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
                version: 2,
            },
            contract_redeemscript: ScriptBuf::default(),
            hashlock_privkey: secp256k1::SecretKey::from_str(
                "0000000000000000000000000000000000000000000000000000000000000004",
            )
            .unwrap(),
            funding_amount: 100_000,
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
                    txid: incoming_swapcoin.contract_tx.txid(),
                    vout: 0, //contract_tx is one-input-one-output
                },
                sequence: Sequence(1), //hashlock spends must have 1 because of the `OP_CSV 1`
                witness: Witness::new(),
                script_sig: ScriptBuf::new(),
            }],
            output: vec![TxOut {
                script_pubkey: destination_address.script_pubkey(),
                value: incoming_swapcoin.contract_tx.output[0].value - miner_fee,
            }],
            lock_time: LockTime::ZERO,
            version: 2,
        };
        let index = 0;
        let input_value = 100;
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

use bitcoin::{
    secp256k1::{self, Secp256k1, SecretKey, Signature},
    util::bip143::SigHashCache,
    Address, OutPoint, PublicKey, Script, SigHashType, Transaction, TxIn, TxOut,
};

use crate::{
    error::TeleportError,
    protocol::{
        contract::{
            apply_two_signatures_to_2of2_multisig_spend, create_multisig_redeemscript,
            read_hashlock_pubkey_from_contract, read_hashvalue_from_contract,
            read_locktime_from_contract, read_pubkeys_from_multisig_redeemscript,
            read_timelock_pubkey_from_contract, sign_contract_tx, verify_contract_tx_sig,
        },
        messages::Preimage,
        Hash160,
    },
};

//swapcoins are UTXOs + metadata which are not from the deterministic wallet
//they are made in the process of a coinswap
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct IncomingSwapCoin {
    pub my_privkey: SecretKey,
    pub other_pubkey: PublicKey,
    pub other_privkey: Option<SecretKey>,
    pub contract_tx: Transaction,
    pub contract_redeemscript: Script,
    pub hashlock_privkey: SecretKey,
    pub funding_amount: u64,
    pub others_contract_sig: Option<Signature>,
    pub hash_preimage: Option<Preimage>,
}

//swapcoins are UTXOs + metadata which are not from the deterministic wallet
//they are made in the process of a coinswap
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct OutgoingSwapCoin {
    pub my_privkey: SecretKey,
    pub other_pubkey: PublicKey,
    pub contract_tx: Transaction,
    pub contract_redeemscript: Script,
    pub timelock_privkey: SecretKey,
    pub funding_amount: u64,
    pub others_contract_sig: Option<Signature>,
    pub hash_preimage: Option<Preimage>,
}

//like the Incoming/OutgoingSwapCoin structs but no privkey or signature information
//used by the taker to monitor coinswaps between two makers
#[derive(Debug, Clone)]
pub struct WatchOnlySwapCoin {
    pub sender_pubkey: PublicKey,
    pub receiver_pubkey: PublicKey,
    pub contract_tx: Transaction,
    pub contract_redeemscript: Script,
    pub funding_amount: u64,
}

pub trait SwapCoin {
    fn get_multisig_redeemscript(&self) -> Script;
    fn get_contract_tx(&self) -> Transaction;
    fn get_contract_redeemscript(&self) -> Script;
    fn get_timelock_pubkey(&self) -> PublicKey;
    fn get_timelock(&self) -> u16;
    fn get_hashlock_pubkey(&self) -> PublicKey;
    fn get_hashvalue(&self) -> Hash160;
    fn get_funding_amount(&self) -> u64;
    fn verify_contract_tx_receiver_sig(&self, sig: &Signature) -> bool;
    fn verify_contract_tx_sender_sig(&self, sig: &Signature) -> bool;
    fn apply_privkey(&mut self, privkey: SecretKey) -> Result<(), TeleportError>;
}

pub trait WalletSwapCoin: SwapCoin {
    fn get_my_pubkey(&self) -> PublicKey;
    fn get_other_pubkey(&self) -> &PublicKey;
    fn get_fully_signed_contract_tx(&self) -> Transaction;
    fn is_hash_preimage_known(&self) -> bool;
}

macro_rules! impl_walletswapcoin {
    ($coin:ident) => {
        impl WalletSwapCoin for $coin {
            fn get_my_pubkey(&self) -> bitcoin::PublicKey {
                let secp = Secp256k1::new();
                PublicKey {
                    compressed: true,
                    key: secp256k1::PublicKey::from_secret_key(&secp, &self.my_privkey),
                }
            }

            fn get_other_pubkey(&self) -> &PublicKey {
                &self.other_pubkey
            }

            fn get_fully_signed_contract_tx(&self) -> Transaction {
                if self.others_contract_sig.is_none() {
                    panic!("invalid state: others_contract_sig not known");
                }
                let my_pubkey = self.get_my_pubkey();
                let multisig_redeemscript =
                    create_multisig_redeemscript(&my_pubkey, &self.other_pubkey);
                let index = 0;
                let secp = Secp256k1::new();
                let sighash = secp256k1::Message::from_slice(
                    &SigHashCache::new(&self.contract_tx).signature_hash(
                        index,
                        &multisig_redeemscript,
                        self.funding_amount,
                        SigHashType::All,
                    )[..],
                )
                .unwrap();
                let sig_mine = secp.sign(&sighash, &self.my_privkey);

                let mut signed_contract_tx = self.contract_tx.clone();
                apply_two_signatures_to_2of2_multisig_spend(
                    &my_pubkey,
                    &self.other_pubkey,
                    &sig_mine,
                    &self.others_contract_sig.unwrap(),
                    &mut signed_contract_tx.input[index],
                    &multisig_redeemscript,
                );
                signed_contract_tx
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
            read_locktime_from_contract(&self.contract_redeemscript).unwrap()
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

        fn get_contract_redeemscript(&self) -> Script {
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
        contract_redeemscript: Script,
        hashlock_privkey: SecretKey,
        funding_amount: u64,
    ) -> Self {
        let secp = Secp256k1::new();
        let hashlock_pubkey = PublicKey {
            compressed: true,
            key: secp256k1::PublicKey::from_secret_key(&secp, &hashlock_privkey),
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
    ) -> Result<(), &'static str> {
        if self.other_privkey.is_none() {
            return Err("unable to sign: incomplete coinswap for this input");
        }
        let secp = Secp256k1::new();
        let my_pubkey = self.get_my_pubkey();

        let sighash = secp256k1::Message::from_slice(
            &SigHashCache::new(tx).signature_hash(
                index,
                redeemscript,
                self.funding_amount,
                SigHashType::All,
            )[..],
        )
        .unwrap();

        let sig_mine = secp.sign(&sighash, &self.my_privkey);
        let sig_other = secp.sign(&sighash, &self.other_privkey.unwrap());

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
    ) {
        let secp = Secp256k1::new();
        let sighash = secp256k1::Message::from_slice(
            &SigHashCache::new(tx).signature_hash(
                index,
                &self.contract_redeemscript,
                input_value,
                SigHashType::All,
            )[..],
        )
        .unwrap();

        let sig_hashlock = secp.sign(&sighash, &self.hashlock_privkey);
        input.witness.push(sig_hashlock.serialize_der().to_vec());
        input.witness[0].push(SigHashType::All as u8);
        input.witness.push(hash_preimage.to_vec());
        input.witness.push(self.contract_redeemscript.to_bytes());
    }

    pub fn sign_hashlocked_transaction_input(
        &self,
        index: usize,
        tx: &Transaction,
        input: &mut TxIn,
        input_value: u64,
    ) {
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
                sequence: 1, //hashlock spends must have 1 because of the `OP_CSV 1`
                witness: Vec::new(),
                script_sig: Script::new(),
            }],
            output: vec![TxOut {
                script_pubkey: destination_address.script_pubkey(),
                value: self.contract_tx.output[0].value - miner_fee,
            }],
            lock_time: 0,
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
        );
        tx
    }

    pub fn verify_contract_tx_sig(&self, sig: &Signature) -> bool {
        verify_contract_tx_sig(
            &self.contract_tx,
            &self.get_multisig_redeemscript(),
            self.funding_amount,
            &self.other_pubkey,
            sig,
        )
    }
}

impl OutgoingSwapCoin {
    pub fn new(
        my_privkey: SecretKey,
        other_pubkey: PublicKey,
        contract_tx: Transaction,
        contract_redeemscript: Script,
        timelock_privkey: SecretKey,
        funding_amount: u64,
    ) -> Self {
        let secp = Secp256k1::new();
        let timelock_pubkey = PublicKey {
            compressed: true,
            key: secp256k1::PublicKey::from_secret_key(&secp, &timelock_privkey),
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
    ) {
        let secp = Secp256k1::new();
        let sighash = secp256k1::Message::from_slice(
            &SigHashCache::new(tx).signature_hash(
                index,
                &self.contract_redeemscript,
                input_value,
                SigHashType::All,
            )[..],
        )
        .unwrap();

        let sig_timelock = secp.sign(&sighash, &self.timelock_privkey);
        input.witness.push(sig_timelock.serialize_der().to_vec());
        input.witness[0].push(SigHashType::All as u8);
        input.witness.push(Vec::new());
        input.witness.push(self.contract_redeemscript.to_bytes());
    }

    pub fn create_timelock_spend(&self, destination_address: &Address) -> Transaction {
        let miner_fee = 128 * 1; //128 vbytes x 1 sat/vb, size calculated using testmempoolaccept
        let mut tx = Transaction {
            input: vec![TxIn {
                previous_output: OutPoint {
                    txid: self.contract_tx.txid(),
                    vout: 0, //contract_tx is one-input-one-output
                },
                sequence: self.get_timelock() as u32,
                witness: Vec::new(),
                script_sig: Script::new(),
            }],
            output: vec![TxOut {
                script_pubkey: destination_address.script_pubkey(),
                value: self.contract_tx.output[0].value - miner_fee,
            }],
            lock_time: 0,
            version: 2,
        };
        let index = 0;
        self.sign_timelocked_transaction_input(
            index,
            &tx.clone(),
            &mut tx.input[0],
            self.contract_tx.output[0].value,
        );
        tx
    }

    //"_with_my_privkey" as opposed to with other_privkey
    pub fn sign_contract_tx_with_my_privkey(
        &self,
        contract_tx: &Transaction,
    ) -> Result<Signature, TeleportError> {
        let multisig_redeemscript = self.get_multisig_redeemscript();
        Ok(sign_contract_tx(
            contract_tx,
            &multisig_redeemscript,
            self.funding_amount,
            &self.my_privkey,
        )
        .map_err(|_| TeleportError::Protocol("error with signing contract tx"))?)
    }

    pub fn verify_contract_tx_sig(&self, sig: &Signature) -> bool {
        verify_contract_tx_sig(
            &self.contract_tx,
            &self.get_multisig_redeemscript(),
            self.funding_amount,
            &self.other_pubkey,
            sig,
        )
    }
}

impl WatchOnlySwapCoin {
    pub fn new(
        multisig_redeemscript: &Script,
        receiver_pubkey: PublicKey,
        contract_tx: Transaction,
        contract_redeemscript: Script,
        funding_amount: u64,
    ) -> Result<WatchOnlySwapCoin, TeleportError> {
        let (pubkey1, pubkey2) = read_pubkeys_from_multisig_redeemscript(multisig_redeemscript)
            .ok_or(TeleportError::Protocol(
                "invalid pubkeys in multisig_redeemscript",
            ))?;
        if pubkey1 != receiver_pubkey && pubkey2 != receiver_pubkey {
            return Err(TeleportError::Protocol(
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

    fn get_multisig_redeemscript(&self) -> Script {
        let secp = Secp256k1::new();
        create_multisig_redeemscript(
            &self.other_pubkey,
            &PublicKey {
                compressed: true,
                key: secp256k1::PublicKey::from_secret_key(&secp, &self.my_privkey),
            },
        )
    }

    fn verify_contract_tx_receiver_sig(&self, sig: &Signature) -> bool {
        self.verify_contract_tx_sig(sig)
    }

    fn verify_contract_tx_sender_sig(&self, sig: &Signature) -> bool {
        self.verify_contract_tx_sig(sig)
    }

    fn apply_privkey(&mut self, privkey: SecretKey) -> Result<(), TeleportError> {
        let secp = Secp256k1::new();
        let pubkey = PublicKey {
            compressed: true,
            key: secp256k1::PublicKey::from_secret_key(&secp, &privkey),
        };
        if pubkey != self.other_pubkey {
            return Err(TeleportError::Protocol("not correct privkey"));
        }
        self.other_privkey = Some(privkey);
        Ok(())
    }
}

impl SwapCoin for OutgoingSwapCoin {
    impl_swapcoin_getters!();

    fn get_multisig_redeemscript(&self) -> Script {
        let secp = Secp256k1::new();
        create_multisig_redeemscript(
            &self.other_pubkey,
            &PublicKey {
                compressed: true,
                key: secp256k1::PublicKey::from_secret_key(&secp, &self.my_privkey),
            },
        )
    }

    fn verify_contract_tx_receiver_sig(&self, sig: &Signature) -> bool {
        self.verify_contract_tx_sig(sig)
    }

    fn verify_contract_tx_sender_sig(&self, sig: &Signature) -> bool {
        self.verify_contract_tx_sig(sig)
    }

    fn apply_privkey(&mut self, privkey: SecretKey) -> Result<(), TeleportError> {
        let secp = Secp256k1::new();
        let pubkey = PublicKey {
            compressed: true,
            key: secp256k1::PublicKey::from_secret_key(&secp, &privkey),
        };
        if pubkey == self.other_pubkey {
            Ok(())
        } else {
            Err(TeleportError::Protocol("not correct privkey"))
        }
    }
}

impl SwapCoin for WatchOnlySwapCoin {
    impl_swapcoin_getters!();

    fn apply_privkey(&mut self, privkey: SecretKey) -> Result<(), TeleportError> {
        let secp = Secp256k1::new();
        let pubkey = PublicKey {
            compressed: true,
            key: secp256k1::PublicKey::from_secret_key(&secp, &privkey),
        };
        if pubkey == self.sender_pubkey || pubkey == self.receiver_pubkey {
            Ok(())
        } else {
            Err(TeleportError::Protocol("not correct privkey"))
        }
    }

    fn get_multisig_redeemscript(&self) -> Script {
        create_multisig_redeemscript(&self.sender_pubkey, &self.receiver_pubkey)
    }

    //potential confusion here:
    //verify sender sig uses the receiver_pubkey
    //verify receiver sig uses the sender_pubkey
    fn verify_contract_tx_sender_sig(&self, sig: &Signature) -> bool {
        verify_contract_tx_sig(
            &self.contract_tx,
            &self.get_multisig_redeemscript(),
            self.funding_amount,
            &self.receiver_pubkey,
            sig,
        )
    }

    fn verify_contract_tx_receiver_sig(&self, sig: &Signature) -> bool {
        verify_contract_tx_sig(
            &self.contract_tx,
            &self.get_multisig_redeemscript(),
            self.funding_amount,
            &self.sender_pubkey,
            sig,
        )
    }
}

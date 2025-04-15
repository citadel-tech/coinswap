//! The musig2 APIs
//!
//! This module includes most of the fundamental functions needed for Taproot and MuSig2.
use bitcoin::key::rand;

use secp256k1::musig::{
    new_nonce_pair, AggregatedNonce, AggregatedSignature, KeyAggCache, PartialSignature, PublicNonce, SecretNonce, Session, SessionSecretRand
};
use bitcoin::secp256k1::{ Keypair, Message, PublicKey, Secp256k1, SecretKey, XOnlyPublicKey};
use secp256k1::{pubkey_sort};

/// Generates a new keypair
pub fn generate_new_keypair() -> Keypair {
    let secp = Secp256k1::new();
    Keypair::new(&secp, &mut rand::thread_rng())
}

/// Sort the public keys
pub fn sort_public_keys( pubkeys: &mut Vec<&PublicKey>) {
    let secp = Secp256k1::new();
    pubkey_sort(&secp, pubkeys.as_mut_slice());
}

/// Aggregates the public keys
pub fn aggregate_public_keys(pubkeys: &Vec<&PublicKey>) -> XOnlyPublicKey {
    let secp = Secp256k1::new();
    let musig_key_agg_cache = KeyAggCache::new(&secp, pubkeys.as_slice());
    musig_key_agg_cache.agg_pk()
}

/// Generates a new nonce pair
pub fn generate_new_nonce_pair(
    musig_key_agg_cache: &KeyAggCache,
    pubkey: PublicKey,
    msg: Message,
    extra_rand: Option<[u8; 32]>,
) -> (SecretNonce, PublicNonce) {
    let secp = Secp256k1::new();
    let musig_session_sec_rand = SessionSecretRand::from_rng(&mut rand::thread_rng());
    musig_key_agg_cache.nonce_gen(&secp, musig_session_sec_rand, pubkey, msg, extra_rand)
}

/// Aggregates the nonces
pub fn get_aggregated_nonce(nonces: &Vec<&PublicNonce>) -> AggregatedNonce {
    let secp = Secp256k1::new();
    AggregatedNonce::new(&secp, nonces.as_slice())
}

/// Generates a partial signature
pub fn generate_partial_signature(
    session: &Session,
    sec_nonce: SecretNonce,
    keypair: Keypair,
    musig_key_agg_cache: &KeyAggCache,
) -> PartialSignature {
    let secp = Secp256k1::new();
    session.partial_sign(&secp, sec_nonce, &keypair, musig_key_agg_cache)
}

/// Verifies a partial signature
pub fn verify_partial_signature(
    session: &Session,
    musig_key_agg_cache: &KeyAggCache,
    partial_sign: PartialSignature,
    pub_nonce: PublicNonce,
    pubkey: PublicKey,
) -> bool {
    let secp = Secp256k1::new();
    session.partial_verify(&secp, musig_key_agg_cache, partial_sign, pub_nonce, pubkey)
}

/// Aggregates the partial signatures
pub fn aggregate_partial_signatures(
    partial_sigs: &Vec<&PartialSignature>,
    session: &Session,
) -> AggregatedSignature {
    session.partial_sig_agg(partial_sigs.as_slice())
}

/// Verifies the aggregated signature
pub fn verify_aggregated_signature(
    agg_pk: &XOnlyPublicKey,
    msg_bytes: &[u8],
    aggregated_signature: &AggregatedSignature,
) -> bool {
    let secp = Secp256k1::new();
    aggregated_signature.verify(&secp, agg_pk, msg_bytes).is_ok()
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    #[test]
    fn test_generate_new_keypair() {
        let keypair = generate_new_keypair();
        assert_eq!(keypair.secret_key().secret_bytes().len(), 32);
    }

    #[test]
    fn test_pubkey_sorting() {
        let key1 = PublicKey::from_str("0307e0f418a8a89b9c0d4df2aee4ea9b2bafb9a66a7b0f4201fe8eebe2c81b9a02").unwrap();
        let key2 = PublicKey::from_str("031976fdbdc36610733c2f669d5c02de5c6b4b4f703b96fd66fe293c46f4f70848").unwrap();
        let key3 = PublicKey::from_str("026041c1f212687344faab28711a2e8d26af3dc15c911017b1297328a9f79dbd19").unwrap();
        let mut pubkeys = vec![&key1, &key2, &key3];
        sort_public_keys(&mut pubkeys);
        assert_eq!(pubkeys, vec![&key3, &key1, &key2]);
    }

    // Test vectors from https://github.com/bitcoin/bips/blob/master/bip-0327/vectors/key_agg_vectors.json
    #[test]
    fn test_key_aggregation() {
        let key1 = PublicKey::from_str("02F9308A019258C31049344F85F89D5229B531C845836F99B08601F113BCE036F9").unwrap();
        let key2 = PublicKey::from_str("03DFF1D77F2A671C5F36183726DB2341BE58FEAE1DA2DECED843240F7B502BA659").unwrap();
        let key3 = PublicKey::from_str("023590A94E768F8E1815C2F24B4D80A8E3149316C3518CE7B7AD338368D038CA66").unwrap();
        let pubkeys = vec![&key1, &key2, &key3];
        let agg_pubkey = aggregate_public_keys(&pubkeys);
        assert_eq!(agg_pubkey.to_string().to_uppercase(), "90539EEDE565F5D054F32CC0C220126889ED1E5D193BAF15AEF344FE59D4610C");
    }
}
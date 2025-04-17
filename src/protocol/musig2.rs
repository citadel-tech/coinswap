//! The musig2 APIs
//!
//! This module includes most of the fundamental functions needed for Taproot and MuSig2.
use bitcoin::key::rand;

use secp256k1::musig::{
    new_nonce_pair, AggregatedNonce, AggregatedSignature, KeyAggCache, PartialSignature, PublicNonce, SecretNonce, Session, SessionSecretRand
};
use secp256k1::{pubkey_sort, Keypair, Message, PublicKey, Secp256k1, SecretKey, XOnlyPublicKey};

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

    use bitcoin::key;

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

    #[test]
    fn end_to_end() {
        let key1 = generate_new_keypair();
        let key2 = generate_new_keypair();
        let key3 = generate_new_keypair();
        let key4 = generate_new_keypair();
        println!("Generated keypairs.");

        let message = Message::from_digest([0; 32]);
        println!("Message created: {:?}", message);

        let pubkey1 = key1.public_key();
        let pubkey2 = key2.public_key();
        let pubkey3 = key3.public_key();
        let pubkey4 = key4.public_key();
        println!("Public keys: \n  pubkey1: {:?}\n  pubkey2: {:?}\n  pubkey3: {:?}\n  pubkey4: {:?}", pubkey1, pubkey2, pubkey3, pubkey4);

        let mut pubkeys = vec![&pubkey1, &pubkey2, &pubkey3, &pubkey4];
        sort_public_keys(&mut pubkeys);
        println!("Sorted public keys: {:?}", pubkeys);

        let agg_pubkey = aggregate_public_keys(&pubkeys);
        println!("Aggregated public key: {:?}", agg_pubkey);

        let musig_key_agg_cache = KeyAggCache::new(&Secp256k1::new(), pubkeys.as_slice());
        println!("Key aggregation cache created.");

        let (sec_nonce1, pub_nonce1) = generate_new_nonce_pair(&musig_key_agg_cache, pubkey1, message, None);
        let (sec_nonce2, pub_nonce2) = generate_new_nonce_pair(&musig_key_agg_cache, pubkey2, message, None);
        let (sec_nonce3, pub_nonce3) = generate_new_nonce_pair(&musig_key_agg_cache, pubkey3, message, None);
        let (sec_nonce4, pub_nonce4) = generate_new_nonce_pair(&musig_key_agg_cache, pubkey4, message, None);
        println!("Generated nonce pairs.");
        println!("  sec_nonce1: {:?}\n  sec_nonce2: {:?}\n  sec_nonce3: {:?}\n  sec_nonce4: {:?}", sec_nonce1, sec_nonce2, sec_nonce3, sec_nonce4);
        println!("  pub_nonce1: {:?}\n  pub_nonce2: {:?}\n  pub_nonce3: {:?}\n  pub_nonce4: {:?}", pub_nonce1, pub_nonce2, pub_nonce3, pub_nonce4);

        let agg_nonce = get_aggregated_nonce(&vec![&pub_nonce1, &pub_nonce2, &pub_nonce3, &pub_nonce4]);
        println!("Aggregated nonce: {:?}", agg_nonce);

        let session = Session::new(&Secp256k1::new(), &musig_key_agg_cache, agg_nonce, message);
        println!("Session created.");

        let partial_sig1 = generate_partial_signature(&session, sec_nonce1, key1, &musig_key_agg_cache);
        let partial_sig2 = generate_partial_signature(&session, sec_nonce2, key2, &musig_key_agg_cache);
        let partial_sig3 = generate_partial_signature(&session, sec_nonce3, key3, &musig_key_agg_cache);
        let partial_sig4 = generate_partial_signature(&session, sec_nonce4, key4, &musig_key_agg_cache);
        println!("Generated partial signatures.");
        println!("  partial_sig1: {:?}\n  partial_sig2: {:?}\n  partial_sig3: {:?}\n  partial_sig4: {:?}", partial_sig1, partial_sig2, partial_sig3, partial_sig4);

        let partial_sigs = vec![&partial_sig1, &partial_sig2, &partial_sig3, &partial_sig4];
        let agg_sig = aggregate_partial_signatures(&partial_sigs, &session);
        println!("Aggregated signature: {:?}", agg_sig);

        let msg_bytes = message.as_ref();
        let is_valid = verify_aggregated_signature(&agg_pubkey, msg_bytes, &agg_sig);
        println!("Aggregated signature valid: {}", is_valid);
        assert!(is_valid);

        let is_valid_partial_sig1 = verify_partial_signature(&session, &musig_key_agg_cache, partial_sig1, pub_nonce1, pubkey1);
        let is_valid_partial_sig2 = verify_partial_signature(&session, &musig_key_agg_cache, partial_sig2, pub_nonce2, pubkey2);
        let is_valid_partial_sig3 = verify_partial_signature(&session, &musig_key_agg_cache, partial_sig3, pub_nonce3, pubkey3);
        let is_valid_partial_sig4 = verify_partial_signature(&session, &musig_key_agg_cache, partial_sig4, pub_nonce4, pubkey4);
        println!("Partial signature 1 valid: {}", is_valid_partial_sig1);
        println!("Partial signature 2 valid: {}", is_valid_partial_sig2);
        println!("Partial signature 3 valid: {}", is_valid_partial_sig3);
        println!("Partial signature 4 valid: {}", is_valid_partial_sig4);

        assert!(is_valid_partial_sig1);
        assert!(is_valid_partial_sig2);
        assert!(is_valid_partial_sig3);
        assert!(is_valid_partial_sig4);
    }
}
//! The musig2 APIs
//!
//! This module includes most of the fundamental functions needed for Taproot and MuSig2.
use std::convert::TryFrom;
use std::str::FromStr;

use bitcoin::key::rand;

use secp256k1::musig::{
    new_nonce_pair, AggregatedNonce, AggregatedSignature, KeyAggCache, PartialSignature, PublicNonce, SecretNonce, Session, SessionSecretRand
};
use secp256k1::{pubkey_sort, Keypair, Message, PublicKey, Secp256k1, SecretKey, XOnlyPublicKey};

/// get aggregated public key from two public keys
pub fn get_aggregated_pubkey(pubkey1: &String, pubkey2: &String) -> String {
    let secp = Secp256k1::new();
    let pubkey1 = PublicKey::from_str(pubkey1).unwrap();
    let pubkey2 = PublicKey::from_str(pubkey2).unwrap();
    let mut pubkeys = vec![&pubkey1, &pubkey2];
    pubkey_sort(&secp, pubkeys.as_mut_slice());
    let musig_key_agg_cache = KeyAggCache::new(&secp, pubkeys.as_slice());
    musig_key_agg_cache.agg_pk().to_string()
}

/// get key aggregation cache from a vector of public keys
pub fn get_musig_key_agg_cache(pubkeys: &Vec<&String>) -> KeyAggCache {
    let pubkey1 = PublicKey::from_str(pubkeys[0]).unwrap();
    let pubkey2 = PublicKey::from_str(pubkeys[1]).unwrap();
    let pubkeys = vec![&pubkey1, &pubkey2];
    let secp = Secp256k1::new();
    let mut pubkeys = pubkeys.clone();
    pubkey_sort(&secp, pubkeys.as_mut_slice());
    KeyAggCache::new(&secp, pubkeys.as_slice())
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
    use std::convert::TryInto;

    use bitcoin::{hex::FromHex, key};
    use secp256k1::Scalar;

    use super::*;

    // #[test]
    #[test]
    fn test_taproot() {
        let secp = Secp256k1::new();
        let seckey1_bytes = [53, 126, 153, 168, 20, 2, 57, 61, 57, 192, 65, 188, 170, 70, 195, 245, 0, 137, 135, 59, 128, 104, 181, 90, 187, 118, 160, 138, 217, 172, 220, 56];
        let keypair1 = Keypair::from_seckey_slice(&secp, &seckey1_bytes).unwrap();
        let seckey2_bytes = [87, 32, 109, 105, 102, 136, 254, 135, 248, 148, 13, 5, 127, 89, 5, 64, 49, 245, 51, 224, 211, 94, 101, 150, 225, 7, 68, 134, 79, 188, 167, 235];
        let keypair2 = Keypair::from_seckey_slice(&secp, &seckey2_bytes).unwrap();

        let pubkey1 = keypair1.public_key();
        let pubkey2 = keypair2.public_key();

        let pubkeys = vec![&pubkey1, &pubkey2];

        let pubkey1 = pubkey1.to_string();
        let pubkey2 = pubkey2.to_string();

        let agg_pubkey = get_aggregated_pubkey(&pubkey1, &pubkey2);
        let agg_pubkey = XOnlyPublicKey::from_str(&agg_pubkey).unwrap();
        println!("Aggregated public key: {:?}", agg_pubkey);

        let pubkey1 = PublicKey::from_str(&pubkey1).unwrap();
        let pubkey2 = PublicKey::from_str(&pubkey2).unwrap();

        let mut musig_key_agg_cache = KeyAggCache::new(&secp, pubkeys.as_slice());
        let tweak = Scalar::from_be_bytes(Vec::<u8>::from_hex("712d48c5f50912f30ea973aa8a713e9009960db11d1d896f40996eac1524c8be").unwrap().try_into().unwrap()).unwrap();
        musig_key_agg_cache.pubkey_xonly_tweak_add(&secp, &tweak);
        let message = Message::from_digest(Vec::<u8>::from_hex("d977c6fd2a9a9e43ef9d66171536a0af5e022f76eae397ab69291a3b1f3b52ea").unwrap().try_into().unwrap());

        let (sec_nonce1, pub_nonce1) = generate_new_nonce_pair(&musig_key_agg_cache, pubkey1, message, None);
        let (sec_nonce2, pub_nonce2) = generate_new_nonce_pair(&musig_key_agg_cache, pubkey2, message, None);
        println!("Generated nonce pairs.");

        let agg_nonce = get_aggregated_nonce(&vec![&pub_nonce1, &pub_nonce2]);
        println!("Aggregated nonce: {:?}", agg_nonce);

        let session = Session::new(&secp, &musig_key_agg_cache, agg_nonce, message);
        println!("Session created.");

        let partial_sig1 = generate_partial_signature(&session, sec_nonce1, keypair1, &musig_key_agg_cache);
        let partial_sig2 = generate_partial_signature(&session, sec_nonce2, keypair2, &musig_key_agg_cache);
        println!("Generated partial signatures.");

        let partial_sigs = vec![&partial_sig1, &partial_sig2];
        let agg_sig = aggregate_partial_signatures(&partial_sigs, &session);
        println!("Aggregated signature: {:?}", agg_sig);

    }
}
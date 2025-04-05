//! The musig2 APIs
//!
//! This module includes most of the fundamental functions needed for Taproot and MuSig2.
use bip39::rand::rngs::ThreadRng;
use bitcoin::key::rand;

use secp256k1::musig::{
    new_nonce_pair, AggregatedNonce, AggregatedSignature, KeyAggCache, PartialSignature, PublicNonce, SecretNonce, Session, SessionSecretRand
};
use secp256k1::{pubkey_sort, Keypair, Message, PublicKey, Secp256k1, SecretKey, XOnlyPublicKey};

/// Generates a new keypair
pub fn generate_new_keypair() -> (SecretKey, PublicKey) {
    let mut rng = ThreadRng::default();
    let secp = Secp256k1::new();
    secp.generate_keypair(&mut rng)
}

/// Sort the public keys
pub fn sort_public_keys( pubkeys: &mut Vec<PublicKey>) {
    let secp = Secp256k1::new();
    let mut pubkeys_ref: Vec<&PublicKey> = pubkeys.iter().collect();
    let pubkeys_ref: &mut [&PublicKey] = pubkeys_ref.as_mut_slice();
    pubkey_sort(&secp, pubkeys_ref);
}

/// Aggregates the public keys
pub fn aggregate_public_keys(pubkeys: &mut Vec<PublicKey>) -> XOnlyPublicKey {
    let secp = Secp256k1::new();
    let mut pubkeys_ref: Vec<&PublicKey> = pubkeys.iter().collect();
    let pubkeys_ref: &mut [&PublicKey] = pubkeys_ref.as_mut_slice();
    let musig_key_agg_cache = KeyAggCache::new(&secp, pubkeys_ref);
    musig_key_agg_cache.agg_pk()
}

/// Generates a new nonce pair
pub fn generate_new_nonce_pair(
    musig_key_agg_cache: &KeyAggCache,
    seckey: SecretKey,
    pubkey: PublicKey,
    msg: Message,
) -> (SecretNonce, PublicNonce) {
    let secp = Secp256k1::new();
    let musig_session_sec_rand = SessionSecretRand::from_rng(&mut rand::thread_rng());
    let nonce_pair = new_nonce_pair(
        &secp,
        musig_session_sec_rand,
        Some(musig_key_agg_cache),
        Some(seckey),
        pubkey,
        Some(msg),
        None,
    );
    nonce_pair
}

/// Aggregates the nonces
pub fn aggregate_nonces(nonces: &mut Vec<PublicNonce>) -> AggregatedNonce {
    let secp = Secp256k1::new();
    let nonces_ref: Vec<&PublicNonce> = nonces.iter().collect();
    let nonces_ref = nonces_ref.as_slice();
    AggregatedNonce::new(&secp, nonces_ref)
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
    partial_sigs: &mut Vec<PartialSignature>,
    session: &Session,
) -> AggregatedSignature {
    let partial_sigs_ref: Vec<&PartialSignature> = partial_sigs.iter().collect();
    let partial_sigs_ref = partial_sigs_ref.as_slice();
    session.partial_sig_agg(partial_sigs_ref)
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
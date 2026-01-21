//! The musig2 APIs
//!
//! This module includes most of the fundamental functions needed for Taproot and MuSig2.

use secp256k1::{
    musig::{
        new_nonce_pair, AggregatedNonce, AggregatedSignature, KeyAggCache, PartialSignature,
        PublicNonce, SecretNonce, Session, SessionSecretRand,
    },
    rand, Keypair, Message, PublicKey, Scalar, Secp256k1, XOnlyPublicKey,
};

use crate::protocol::error::ProtocolError;

/// get aggregated public key from two public keys
pub fn get_aggregated_pubkey(pubkey1: &PublicKey, pubkey2: &PublicKey) -> XOnlyPublicKey {
    let secp = Secp256k1::new();
    let mut pubkeys = [pubkey1, pubkey2];
    // Sort pubkeys lexicographically (manual implementation)
    pubkeys.sort_by_key(|a| a.serialize());
    let agg_cache = KeyAggCache::new(&secp, &pubkeys);
    agg_cache.agg_pk()
}

/// Generates a new nonce pair
pub fn generate_new_nonce_pair(pubkey: PublicKey) -> (SecretNonce, PublicNonce) {
    let secp = Secp256k1::new();
    let musig_session_sec_rand = SessionSecretRand::from_rng(&mut rand::thread_rng());
    new_nonce_pair(
        &secp,
        musig_session_sec_rand,
        None,
        None,
        pubkey,
        None,
        None,
    )
}

/// Generates a partial signature
pub fn generate_partial_signature(
    message: Message,
    agg_nonce: &AggregatedNonce,
    sec_nonce: SecretNonce,
    keypair: Keypair,
    tap_tweak: Scalar,
    pubkeys: &[&PublicKey],
) -> Result<PartialSignature, ProtocolError> {
    let secp = Secp256k1::new();
    let mut musig_key_agg_cache = KeyAggCache::new(&secp, pubkeys);
    musig_key_agg_cache.pubkey_xonly_tweak_add(&secp, &tap_tweak)?;
    let session = Session::new(&secp, &musig_key_agg_cache, *agg_nonce, message);
    Ok(session.partial_sign(&secp, sec_nonce, &keypair, &musig_key_agg_cache))
}

/// Aggregates the partial signatures
pub fn aggregate_partial_signatures(
    message: Message,
    agg_nonce: AggregatedNonce,
    tap_tweak: Scalar,
    partial_sigs: &[&PartialSignature],
    pubkeys: &[&PublicKey],
) -> Result<AggregatedSignature, ProtocolError> {
    let secp = Secp256k1::new();
    let mut musig_key_agg_cache = KeyAggCache::new(&secp, pubkeys);
    musig_key_agg_cache.pubkey_xonly_tweak_add(&secp, &tap_tweak)?;
    let session = Session::new(&secp, &musig_key_agg_cache, agg_nonce, message);
    Ok(session.partial_sig_agg(partial_sigs))
}

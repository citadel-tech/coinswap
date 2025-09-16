//! This module provides an interface for the musig2 protocol.
use super::{
    musig2::{aggregate_partial_signatures, generate_partial_signature},
    *,
};
use musig2::{generate_new_nonce_pair, get_aggregated_pubkey};

//TODOS
//- Use macro for converting between secp256k1 and bitcoin::secp256k1
//- Use named imports for secp256k1 and bitcoin::secp256k1

/// Aggregates the public keys
pub fn get_aggregated_pubkey_i(
    pubkey1: bitcoin::secp256k1::PublicKey,
    pubkey2: bitcoin::secp256k1::PublicKey,
) -> bitcoin::secp256k1::XOnlyPublicKey {
    let s_pubkey1 = pubkey1.serialize();
    let s_pubkey2 = pubkey2.serialize();
    let pubkey1 = secp256k1::PublicKey::from_slice(&s_pubkey1).unwrap();
    let pubkey2 = secp256k1::PublicKey::from_slice(&s_pubkey2).unwrap();
    let agg_pubkey = get_aggregated_pubkey(&pubkey1, &pubkey2);
    let s_agg_pubkey = agg_pubkey.serialize();
    let agg_pubkey = bitcoin::secp256k1::XOnlyPublicKey::from_slice(&s_agg_pubkey).unwrap();
    agg_pubkey
}

/// Generates a new nonce pair
pub fn generate_new_nonce_pair_i(
    tap_tweak: bitcoin::secp256k1::Scalar,
    pubkey1: bitcoin::secp256k1::PublicKey,
    pubkey2: bitcoin::secp256k1::PublicKey,
    nonce_pubkey: bitcoin::secp256k1::PublicKey,
    message: bitcoin::secp256k1::Message,
) -> (secp256k1::musig::SecretNonce, secp256k1::musig::PublicNonce) {
    let tap_tweak = tap_tweak.to_be_bytes();
    let tap_tweak = secp256k1::Scalar::from_be_bytes(tap_tweak).unwrap();
    let pubkey1 = pubkey1.serialize();
    let pubkey2 = pubkey2.serialize();
    let pubkey1 = secp256k1::PublicKey::from_slice(&pubkey1).unwrap();
    let pubkey2 = secp256k1::PublicKey::from_slice(&pubkey2).unwrap();
    let pubkeys = vec![&pubkey1, &pubkey2];
    let nonce_pubkey = nonce_pubkey.serialize();
    let nonce_pubkey = secp256k1::PublicKey::from_slice(&nonce_pubkey).unwrap();
    // Convert bitcoin::secp256k1::Message to secp256k1::Message directly from bytes
    let message_bytes = message.as_ref();
    let message = secp256k1::Message::from_digest(*message_bytes);
    generate_new_nonce_pair(tap_tweak, &pubkeys, nonce_pubkey, message, None)
}

/// get aggregated nonce
pub fn get_aggregated_nonce_i(
    nonces: &Vec<&secp256k1::musig::PublicNonce>,
) -> secp256k1::musig::AggregatedNonce {
    let secp = secp256k1::Secp256k1::new();
    secp256k1::musig::AggregatedNonce::new(&secp, nonces.as_slice())
}

/// Generates a partial signature
pub fn generate_partial_signature_i(
    message: bitcoin::secp256k1::Message,
    agg_nonce: &secp256k1::musig::AggregatedNonce,
    sec_nonce: secp256k1::musig::SecretNonce,
    keypair: bitcoin::secp256k1::Keypair,
    tap_tweak: bitcoin::secp256k1::Scalar,
    pubkey1: bitcoin::secp256k1::PublicKey,
    pubkey2: bitcoin::secp256k1::PublicKey,
) -> secp256k1::musig::PartialSignature {
    let secp = secp256k1::Secp256k1::new();
    let tap_tweak = tap_tweak.to_be_bytes();
    let tap_tweak = secp256k1::Scalar::from_be_bytes(tap_tweak).unwrap();
    let pubkey1 = pubkey1.serialize();
    let pubkey2 = pubkey2.serialize();
    let pubkey1 = secp256k1::PublicKey::from_slice(&pubkey1).unwrap();
    let pubkey2 = secp256k1::PublicKey::from_slice(&pubkey2).unwrap();
    let pubkeys = vec![&pubkey1, &pubkey2];
    // Convert bitcoin::secp256k1::Message to secp256k1::Message directly from bytes
    let message_bytes = message.as_ref();
    let message = secp256k1::Message::from_digest(*message_bytes);
    let keypair_secret = keypair.secret_bytes();
    let secret_key = secp256k1::SecretKey::from_slice(&keypair_secret).unwrap();
    let keypair = secp256k1::Keypair::from_secret_key(&secp, &secret_key);
    generate_partial_signature(message, agg_nonce, sec_nonce, keypair, tap_tweak, &pubkeys)
}

/// Aggregates the partial signatures
pub fn aggregate_partial_signatures_i(
    message: bitcoin::secp256k1::Message,
    agg_nonce: secp256k1::musig::AggregatedNonce,
    tap_tweak: bitcoin::secp256k1::Scalar,
    partial_sigs: Vec<&secp256k1::musig::PartialSignature>,
    pubkey_1: bitcoin::secp256k1::PublicKey,
    pubkey2: bitcoin::secp256k1::PublicKey,
) -> secp256k1::musig::AggregatedSignature {
    let tap_tweak = tap_tweak.to_be_bytes();
    let tap_tweak = secp256k1::Scalar::from_be_bytes(tap_tweak).unwrap();
    // Convert bitcoin::secp256k1::Message to secp256k1::Message directly from bytes
    let message_bytes = message.as_ref();
    let message = secp256k1::Message::from_digest(*message_bytes);
    let pubkey1 = pubkey_1.serialize();
    let pubkey2 = pubkey2.serialize();
    let pubkey1 = secp256k1::PublicKey::from_slice(&pubkey1).unwrap();
    let pubkey2 = secp256k1::PublicKey::from_slice(&pubkey2).unwrap();
    let pubkeys = [&pubkey1, &pubkey2];
    aggregate_partial_signatures(message, agg_nonce, tap_tweak, &partial_sigs, &pubkeys)
}

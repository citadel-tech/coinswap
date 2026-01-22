//! This module provides an interface for the musig2 protocol.
use crate::protocol::error::ProtocolError;

use super::{
    musig2::{aggregate_partial_signatures, generate_partial_signature},
    *,
};
use bitcoin::secp256k1 as btc_secp;
use musig2::{generate_new_nonce_pair, get_aggregated_pubkey};
use secp256k1 as secp;

/// Convert a bitcoin::secp256k1 public key into secp256k1 public key
macro_rules! btc_pubkey_to_secp {
    ($pk:expr) => {
        secp::PublicKey::from_slice(&$pk.serialize())?
    };
}

/// Convert a bitcoin::secp256k1 scalar into secp256k1 scalar
macro_rules! btc_scalar_to_secp {
    ($scalar:expr) => {
        secp::Scalar::from_be_bytes($scalar.to_be_bytes())?
    };
}

/// Convert bitcoin::secp256k1::Message to secp256k1::Message
macro_rules! btc_msg_to_secp {
    ($msg:expr) => {
        secp::Message::from_digest(*$msg.as_ref())
    };
}

/// Aggregates the public keys
pub fn get_aggregated_pubkey_compat(
    pubkey1: btc_secp::PublicKey,
    pubkey2: btc_secp::PublicKey,
) -> Result<btc_secp::XOnlyPublicKey, ProtocolError> {
    let pubkey1 = btc_pubkey_to_secp!(pubkey1);
    let pubkey2 = btc_pubkey_to_secp!(pubkey2);
    let agg_pubkey = get_aggregated_pubkey(&pubkey1, &pubkey2);
    Ok(btc_secp::XOnlyPublicKey::from_slice(
        &agg_pubkey.serialize(),
    )?)
}

/// Generates a new nonce pair
pub fn generate_new_nonce_pair_compat(
    nonce_pubkey: btc_secp::PublicKey,
) -> Result<(secp::musig::SecretNonce, secp::musig::PublicNonce), ProtocolError> {
    let nonce_pubkey = btc_pubkey_to_secp!(nonce_pubkey);
    Ok(generate_new_nonce_pair(nonce_pubkey))
}

/// Get aggregated nonce
pub fn get_aggregated_nonce_compat(
    nonces: &[&secp::musig::PublicNonce],
) -> secp::musig::AggregatedNonce {
    let secp = secp::Secp256k1::new();
    secp::musig::AggregatedNonce::new(&secp, nonces)
}

/// Generates a partial signature
pub fn generate_partial_signature_compat(
    message: btc_secp::Message,
    agg_nonce: &secp::musig::AggregatedNonce,
    sec_nonce: secp::musig::SecretNonce,
    keypair: btc_secp::Keypair,
    tap_tweak: btc_secp::Scalar,
    pubkey1: btc_secp::PublicKey,
    pubkey2: btc_secp::PublicKey,
) -> Result<secp::musig::PartialSignature, ProtocolError> {
    let secp = secp::Secp256k1::new();
    let message = btc_msg_to_secp!(message);
    let tap_tweak = btc_scalar_to_secp!(tap_tweak);
    let pubkey1 = btc_pubkey_to_secp!(pubkey1);
    let pubkey2 = btc_pubkey_to_secp!(pubkey2);
    let pubkeys = [&pubkey1, &pubkey2];
    let secret_key = secp::SecretKey::from_slice(&keypair.secret_bytes())?;
    let keypair = secp::Keypair::from_secret_key(&secp, &secret_key);

    generate_partial_signature(message, agg_nonce, sec_nonce, keypair, tap_tweak, &pubkeys)
}

/// Aggregates the partial signatures
pub fn aggregate_partial_signatures_compat(
    message: btc_secp::Message,
    agg_nonce: secp::musig::AggregatedNonce,
    tap_tweak: btc_secp::Scalar,
    partial_sigs: Vec<&secp::musig::PartialSignature>,
    pubkey1: btc_secp::PublicKey,
    pubkey2: btc_secp::PublicKey,
) -> Result<secp::musig::AggregatedSignature, ProtocolError> {
    let message = btc_msg_to_secp!(message);
    let tap_tweak = btc_scalar_to_secp!(tap_tweak);
    let pubkey1 = btc_pubkey_to_secp!(pubkey1);
    let pubkey2 = btc_pubkey_to_secp!(pubkey2);

    aggregate_partial_signatures(
        message,
        agg_nonce,
        tap_tweak,
        &partial_sigs,
        &[&pubkey1, &pubkey2],
    )
}

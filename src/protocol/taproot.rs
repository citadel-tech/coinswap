/// Aggregates two public keys using the MuSig key aggregation scheme.
///
/// This function takes two public keys and aggregates them using the
/// MuSig key aggregation scheme, producing a `MusigKeyAggCache` which
/// can be used in multi-signature protocols.
///
/// # Arguments
///
/// * `pub_key1` - The first public key.
/// * `pub_key2` - The second public key.
///
/// # Returns
///
/// A `MusigKeyAggCache` object containing the aggregated public key.
///
/// # Example
///
///
/// ```rust
/// # # [cfg(any(test, feature = "rand-std"))] {
/// # use secp256k1_zkp::rand::{thread_rng, RngCore};
/// # use secp256k1_zkp::{MusigKeyAggCache, Secp256k1, SecretKey, Keypair, PublicKey};
/// # use coinswap::protocol::taproot::key_agg_cache;
/// # let secp = Secp256k1::new();
/// # let sk1 = SecretKey::new(&mut thread_rng());
/// # let pub_key1 = PublicKey::from_secret_key(&secp, &sk1);
/// # let sk2 = SecretKey::new(&mut thread_rng());
/// # let pub_key2 = PublicKey::from_secret_key(&secp, &sk2);
/// #
/// let key_agg_cache = key_agg_cache(pub_key1, pub_key2);
///
/// # }
/// ```
///
/// # Panics
///
/// This function will panic if the provided public keys are not valid.
///
/// # Safety
///
/// This function is safe to use as long as valid public keys are provided.
///
/// # Notes
///
/// The order of the public keys does not affect the aggregated result,
/// as the function internally sorts them before aggregation.
pub fn key_agg_cache(
    pub_key1: secp256k1_zkp::PublicKey,
    pub_key2: secp256k1_zkp::PublicKey,
) -> secp256k1_zkp::MusigKeyAggCache {
    let secp = secp256k1_zkp::Secp256k1::new();
    let mut arr_pub: Vec<secp256k1_zkp::PublicKey> = vec![pub_key1, pub_key2];
    arr_pub.push(pub_key1);
    arr_pub.push(pub_key2);
    arr_pub.sort();

    secp256k1_zkp::MusigKeyAggCache::new(&secp, &[arr_pub[0], arr_pub[1]])
}

/// Generates a secret nonce and public nonce for a given message and public key using the MuSig scheme.
///
/// This function generates a pair of nonces, one secret and one public, for a given message and public key.
/// The nonces are generated in the context of a MuSig multi-signature scheme using the provided
/// `MusigKeyAggCache`, which contains the aggregated public key.
///
/// # Arguments
///
/// * `pub_key1` - The public key of the participant generating the nonce.
/// * `msg` - The message to be signed.
/// * `key_agg_cache` - The `MusigKeyAggCache` containing the aggregated public key.
///
/// # Returns
///
/// A tuple containing:
/// * `MusigSecNonce` - The secret nonce.
/// * `MusigPubNonce` - The public nonce.
///
/// # Example
///
/// ```rust
/// # # [cfg(any(test, feature = "rand-std"))] {
/// # use secp256k1_zkp::rand::{thread_rng, RngCore};
/// # use secp256k1_zkp::{MusigKeyAggCache, Secp256k1, SecretKey, Keypair, PublicKey, MusigSessionId, Message};
/// # let secp = Secp256k1::new();
/// # let sk1 = SecretKey::new(&mut thread_rng());
/// # let pub_key1 = PublicKey::from_secret_key(&secp, &sk1);
/// # let sk2 = SecretKey::new(&mut thread_rng());
/// # let pub_key2 = PublicKey::from_secret_key(&secp, &sk2);
/// #
/// let key_agg_cache = key_agg_cache(pub_key1, pub_key2);
/// let msg = Message::from_digest_slice(b"Public Message we want to sign!!").unwrap();
/// let (_sec_nonce, _pub_nonce) = nonce_gen(pub_key1, msg, key_agg_cache);
/// # }
/// ```
///
/// # Panics
///
/// This function will panic if the nonce generation fails, which should only occur if the session ID is not unique.
///
/// # Safety
///
/// Ensure that the session ID is unique per nonce generation, as non-unique IDs may compromise the security
/// of the MuSig scheme. The session ID is randomly generated in this function.
///
/// # Notes
///
/// The generated nonces are crucial for the MuSig signing process. The secret nonce must be kept confidential,
/// while the public nonce is shared with other participants.
pub fn nonce_gen(
    pub_key1: secp256k1_zkp::PublicKey,
    msg: secp256k1_zkp::Message,
    key_agg_cache: secp256k1_zkp::MusigKeyAggCache,
) -> (secp256k1_zkp::MusigSecNonce, secp256k1_zkp::MusigPubNonce) {
    let secp = secp256k1_zkp::Secp256k1::new();
    // The session id must be sampled at random. Read documentation for more details.
    let session_id1 = secp256k1_zkp::MusigSessionId::assume_unique_per_nonce_gen(
        bitcoin::secp256k1::rand::random(),
    );
    let (sec_nonce, pub_nonce): (secp256k1_zkp::MusigSecNonce, secp256k1_zkp::MusigPubNonce) =
        key_agg_cache
            .nonce_gen(&secp, session_id1, pub_key1, msg, None)
            .expect("non zero session id");
    (sec_nonce, pub_nonce)
}

/// Generates a partial signature in the MuSig multi-signature scheme.
///
/// This function generates a partial signature for a given message, using the signer's secret key,
/// their secret nonce, and the public nonces of all participants. The `MusigKeyAggCache` is used
/// to aggregate the public keys and other required data for the MuSig scheme.
///
/// # Arguments
///
/// * `sec_key` - The secret key of the participant generating the partial signature.
/// * `sec_nonce` - The secret nonce of the participant.
/// * `pub_nonce1` - The public nonce of the first participant.
/// * `pub_nonce2` - The public nonce of the second participant.
/// * `msg` - The message to be signed.
/// * `key_agg_cache` - The `MusigKeyAggCache` containing the aggregated public key.
///
/// # Returns
///
/// A `MusigPartialSignature` which can be combined with other partial signatures to form a complete signature.
///
/// # Example
/// ```rust
/// # # [cfg(any(test, feature = "rand-std"))] {
/// # use secp256k1_zkp::rand::{thread_rng, RngCore};
/// # use secp256k1_zkp::{MusigKeyAggCache, Secp256k1, SecretKey, Keypair, PublicKey, MusigSessionId, Message};
/// # let secp = Secp256k1::new();
/// # let sk1 = SecretKey::new(&mut thread_rng());
/// # let pub_key1 = PublicKey::from_secret_key(&secp, &sk1);
/// # let sk2 = SecretKey::new(&mut thread_rng());
/// # let pub_key2 = PublicKey::from_secret_key(&secp, &sk2);
/// #
/// let key_agg_cache = key_agg_cache(pub_key1, pub_key2);
///
/// let msg1 = Message::from_digest_slice(b"Public Message one we want to sign!!").unwrap();
/// let (sec_nonce1, pub_nonce1) = nonce_gen(pub_key1, msg1, key_agg_cache);
///
/// let msg2 = Message::from_digest_slice(b"Public Message two we want to sign!!").unwrap();
/// let (_sec_nonce2, pub_nonce2) = nonce_gen(pub_key2, msg2, key_agg_cache);
///
/// let partial_sig = partial_signature_gen(sk1, sec_nonce1, pub_nonce1, pub_nonce2, msg1, key_agg_cache);
/// # }
/// ```
///
/// # Panics
///
/// This function will panic if the partial signature generation fails.
///
/// # Safety
///
/// The secret key and secret nonce must be kept confidential. Exposure of these secrets may compromise the security
/// of the multi-signature process.
///
/// # Notes
///
/// The generated partial signature must be combined with other participants' partial signatures to form a complete
/// signature. The order of public nonces should match the order of public keys used in the key aggregation.
pub fn partial_signature_gen(
    sec_key: secp256k1_zkp::SecretKey,
    sec_nonce: secp256k1_zkp::MusigSecNonce,
    pub_nonce1: secp256k1_zkp::MusigPubNonce,
    pub_nonce2: secp256k1_zkp::MusigPubNonce,
    msg: secp256k1_zkp::Message,
    key_agg_cache: secp256k1_zkp::MusigKeyAggCache,
) -> secp256k1_zkp::MusigPartialSignature {
    let secp = secp256k1_zkp::Secp256k1::new();
    let keypair: secp256k1_zkp::Keypair = secp256k1_zkp::Keypair::from_secret_key(&secp, &sec_key);
    let aggnonce = secp256k1_zkp::MusigAggNonce::new(&secp, &[pub_nonce1, pub_nonce2]);
    let session = secp256k1_zkp::MusigSession::new(&secp, &key_agg_cache, aggnonce, msg);
    let partial_sig: secp256k1_zkp::MusigPartialSignature = session
        .partial_sign(&secp, sec_nonce, &keypair, &key_agg_cache)
        .expect("invail parameters");

    partial_sig
}

/// Combines partial signatures to produce a complete Schnorr signature in the MuSig multi-signature scheme.
///
/// This function combines a participant's own partial signature with another participant's partial signature
/// to produce a complete Schnorr signature for a given message. The function requires the participant's secret key,
/// secret nonce, public nonces of all participants, the message, and the `MusigKeyAggCache`.
///
/// # Arguments
///
/// * `partial_sig2` - The partial signature from another participant.
/// * `sec_key` - The secret key of the participant generating the complete signature.
/// * `sec_nonce` - The secret nonce of the participant.
/// * `pub_nonce1` - The public nonce of the first participant.
/// * `pub_nonce2` - The public nonce of the second participant.
/// * `msg` - The message to be signed.
/// * `key_agg_cache` - The `MusigKeyAggCache` containing the aggregated public key.
///
/// # Returns
///
/// A `schnorr::Signature` which is the complete signature for the message.
///
/// # Example
///
/// ```rust
/// # # [cfg(any(test, feature = "rand-std"))] {
/// # use secp256k1_zkp::rand::{thread_rng, RngCore};
/// # use secp256k1_zkp::{MusigKeyAggCache, Secp256k1, SecretKey, Keypair, PublicKey, MusigSessionId, Message};
/// # let secp = Secp256k1::new();
/// # let sk1 = SecretKey::new(&mut thread_rng());
/// # let pub_key1 = PublicKey::from_secret_key(&secp, &sk1);
/// # let sk2 = SecretKey::new(&mut thread_rng());
/// # let pub_key2 = PublicKey::from_secret_key(&secp, &sk2);
/// #
/// let key_agg_cache = key_agg_cache(pub_key1, pub_key2);
///
/// let msg1 = Message::from_digest_slice(b"Public Message one we want to sign!!").unwrap();
/// let (sec_nonce1, pub_nonce1) = nonce_gen(pub_key1, msg1, key_agg_cache);
///
/// let msg2 = Message::from_digest_slice(b"Public Message two we want to sign!!").unwrap();
/// let (_sec_nonce2, pub_nonce2) = nonce_gen(pub_key2, msg2, key_agg_cache);
///
/// let partial_sig1 = partial_signature_gen(sk1, sec_nonce1, pub_nonce1, pub_nonce2, msg1, key_agg_cache);
/// let partial_sig2 = partial_signature_gen(sk2, sec_nonce2, pub_nonce1, pub_nonce2, msg2, key_agg_cache);
///
/// let schnorr_sig = musig_signature(partial_sig1, partial_sig2, pub_nonce1, pub_nonce2, msg1, key_agg_cache);
/// # }
/// ```
/// # Panics
///
/// This function will panic if the combination of partial signatures fails.
///
/// # Safety
///
/// The secret key and secret nonce must be kept confidential. Exposure of these secrets may compromise the security
/// of the multi-signature process.
///
/// # Notes
///
/// The complete signature can be verified using the aggregated public key from the `MusigKeyAggCache`.
/// Ensure the order of public nonces matches the order of public keys used in the key aggregation.
pub fn musig_signature_agg(
    partial_sig1: secp256k1_zkp::MusigPartialSignature,
    partial_sig2: secp256k1_zkp::MusigPartialSignature,
    pub_nonce1: secp256k1_zkp::MusigPubNonce,
    pub_nonce2: secp256k1_zkp::MusigPubNonce,
    msg: secp256k1_zkp::Message,
    key_agg_cache: secp256k1_zkp::MusigKeyAggCache,
) -> secp256k1_zkp::schnorr::Signature {
    let secp = secp256k1_zkp::Secp256k1::new();
    let aggnonce = secp256k1_zkp::MusigAggNonce::new(&secp, &[pub_nonce1, pub_nonce2]);
    let session = secp256k1_zkp::MusigSession::new(&secp, &key_agg_cache, aggnonce, msg);
    session.partial_sig_agg(&[partial_sig1, partial_sig2])
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::secp256k1::rand::{self};

    fn generate_keypair() -> (bitcoin::secp256k1::SecretKey, bitcoin::secp256k1::PublicKey) {
        let secp = bitcoin::key::Secp256k1::new();
        let (secret_key, public_key) = secp.generate_keypair(&mut rand::thread_rng());
        (secret_key, public_key)
    }

    #[test]
    fn test_key_agg_cache_different_order() {
        let (_sk1, pk1) = generate_keypair();
        let (_sk2, pk2) = generate_keypair();

        let pk_ser1 = pk1.serialize_uncompressed();
        let pk_ser2 = pk2.serialize_uncompressed();

        let pk1_zkp = secp256k1_zkp::PublicKey::from_slice(&pk_ser1).unwrap();
        let pk2_zkp = secp256k1_zkp::PublicKey::from_slice(&pk_ser2).unwrap();

        let cache1 = key_agg_cache(pk1_zkp, pk2_zkp);
        let cache2 = key_agg_cache(pk2_zkp, pk1_zkp);

        // Verify that cache results are identical irrespective of the order of public keys
        assert_eq!(cache1, cache2);
    }
}

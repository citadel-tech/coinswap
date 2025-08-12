//! Generic crate for securely loading and storing sensitive data from disk.
//!
//! This module provides utilities for encrypting and decrypting serializable Rust structs,
//! using AES-256-GCM encryption and CBOR/JSON serialization formats.
//!
//! The main focus is securely storing wallet-related or sensitive data structures,
//! allowing seamless support for both encrypted and unencrypted files.

use aes_gcm::{
    aead::{Aead, OsRng},
    AeadCore, Aes256Gcm, Key, KeyInit,
};
use bip39::rand::random;
use pbkdf2::pbkdf2_hmac_array;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::Sha256;

use crate::utill;
use std::{fs, path::Path};

/// Errors that can occur during the encryption process.
///
/// This enum covers serialization errors from CBOR encoding as well as
/// encryption failures.
#[derive(Debug)]
pub enum EncryptError {
    /// Error occurred during CBOR serialization of the input struct.
    Serialization(serde_cbor::Error),
    /// Error occurred during AES-GCM encryption.
    ///
    /// Note: This error type carries no additional information because
    /// the underlying AES-GCM error is a zero-sized marker.
    Encryption,
}

impl From<serde_cbor::Error> for EncryptError {
    fn from(err: serde_cbor::Error) -> Self {
        EncryptError::Serialization(err)
    }
}

impl From<aes_gcm::Error> for EncryptError {
    fn from(_: aes_gcm::Error) -> Self {
        EncryptError::Encryption
    }
}

/// Represents a deserialization format used for loading wallet files from disk.
///
/// This trait abstracts over format-specific parsing logic to allow runtime
/// switching between JSON and CBOR.
///
/// It is specifically used by [`load_sensitive_struct_interactive`] to try parsing
/// the file in both formats.
///
/// **Note on CBOR:**  
/// Due to potential trailing bytes left by `serde_cbor`, the CBOR variant delegates
/// parsing to [`utill::deserialize_from_cbor`] that strips extra data before deserialization.
///
/// This trait is **not** intended for general-purpose serialization or format negotiation.
pub trait SerdeFormat {
    /// Errors that can occur while parsing or deserializing data
    /// using this serialization format.
    ///
    /// Implementors should use a concrete error type that describes
    /// failures specific to their format (e.g., `serde_json::Error`).
    type Error: std::error::Error + Send + Sync + 'static;

    #[allow(missing_docs)]
    fn from_slice<T: DeserializeOwned>(input: &[u8]) -> Result<T, Self::Error>;
}
/// JSON implementation of `SerdeFormat`, using `serde_json`.
///
/// Used for loading plain or encrypted wallet files that were serialized as JSON.
pub struct SerdeJson;

impl SerdeFormat for SerdeJson {
    type Error = serde_json::Error;
    fn from_slice<T: DeserializeOwned>(input: &[u8]) -> Result<T, Self::Error> {
        serde_json::from_slice(input)
    }
}
/// CBOR implementation of `SerdeFormat`, using a utility wrapper
/// that handles CBOR trailing data properly.
///
/// `serde_cbor` may leave trailing data behind, which can cause
/// parsing errors.
/// This wrapper [`utill::deserialize_from_cbor`] utility method to cleanly deserialize.
pub struct SerdeCbor;

impl SerdeFormat for SerdeCbor {
    type Error = serde_cbor::Error;
    fn from_slice<T: DeserializeOwned>(input: &[u8]) -> Result<T, Self::Error> {
        utill::deserialize_from_cbor::<T>(input.to_vec())
    }
}
/// A 16-byte (128-bit) salt used with PBKDF2 to derive encryption keys.
///
/// This salt is randomly generated each time new encryption is performed, ensuring that
/// even if two users choose the same password, their derived keys will be unique.
/// The combination of the user’s password and this salt produces a unique encryption key.
///
/// 16 bytes (128 bits) is recommended to provide sufficient entropy to prevent precomputation attacks.
/// See:
/// - https://datatracker.ietf.org/doc/html/rfc2898#section-5.2
/// - https://docs.rs/password-hash/0.5.0/password_hash/struct.Salt.html#associatedconstant.RECOMMENDED_LENGTH
type PBKDF2Salt = [u8; 16];
/// A 12-byte (96-bit) nonce used as the Initialization Vector (IV) for AES-GCM encryption.
type EncryptionNonce = [u8; 12];
/// A 32-byte (256-bit) key derived from a passphrase via PBKDF2,
/// used as the symmetric encryption key with AES-GCM.
type EncryptionKey = [u8; 32];

/// Number of PBKDF2 iterations to strengthen passphrase-derived keys.
///
/// In production, this is set to **600,000 iterations**, following
/// modern password security guidance from the
/// [OWASP Password Storage Cheat Sheet](https://cheatsheetseries.owasp.org/cheatsheets/Password_Storage_Cheat_Sheet.html).
///
/// During testing or integration tests, the iteration count is reduced to 1
/// for performance.
const PBKDF2_ITERATIONS: u32 = if cfg!(feature = "integration-test") || cfg!(test) {
    1
} else {
    600_000
};

/// Holds derived cryptographic key material used for encrypting and decrypting wallet data.
#[derive(Debug, Clone)]
pub struct KeyMaterial {
    /// A 256-bit key derived from the user’s passphrase via PBKDF2.
    /// This key is used with AES-GCM for encryption/decryption.
    pub key: EncryptionKey,
    /// Nonce used for AES-GCM encryption, generated when a new wallet is created.
    pub nonce: EncryptionNonce,
    /// Key derivation salt, randomly generated to ensure unique keys per password.
    pub pbkdf2_salt: PBKDF2Salt,
}
impl KeyMaterial {
    /// Creates new key material from a password, with a freshly random generated nonce and salt.
    pub fn new_from_password(password: String) -> Self {
        let pbkdf2_salt = random::<PBKDF2Salt>();
        KeyMaterial {
            key: pbkdf2_hmac_array::<Sha256, 32>(
                password.as_bytes(),
                &pbkdf2_salt,
                PBKDF2_ITERATIONS,
            ),
            nonce: Aes256Gcm::generate_nonce(&mut OsRng).into(),
            pbkdf2_salt,
        }
    }
    /// Prompts the user interactively for a new encryption passphrase.
    ///
    /// If the user enters an empty string, returns `None`, indicating no encryption.
    /// Otherwise, returns `Some(KeyMaterial)` with a newly generated nonce and salt.
    pub fn new_interactive(prompt: Option<String>) -> Option<Self> {
        let enc_password =
            utill::prompt_password(prompt.unwrap_or(
                "Enter new encryption passphrase (empty for no encryption): ".to_string(),
            ))
            .unwrap();

        if enc_password.is_empty() {
            None
        } else {
            let pbkdf2_salt = random::<PBKDF2Salt>();
            Some(KeyMaterial {
                key: pbkdf2_hmac_array::<Sha256, 32>(
                    enc_password.as_bytes(),
                    &pbkdf2_salt,
                    PBKDF2_ITERATIONS,
                ),
                nonce: Aes256Gcm::generate_nonce(&mut OsRng).into(),
                pbkdf2_salt,
            })
        }
    }

    /// Creates a complete `KeyMaterial` from a password, a known nonce and a known salt.
    ///
    /// This is used when decrypting existing wallet data, where the nonce
    /// and the salt have already been read from disk and are available.
    pub fn existing(password: String, nonce: EncryptionNonce, pbkdf2_salt: PBKDF2Salt) -> Self {
        KeyMaterial {
            key: pbkdf2_hmac_array::<Sha256, 32>(
                password.as_bytes(),
                &pbkdf2_salt,
                PBKDF2_ITERATIONS,
            ),
            nonce,
            pbkdf2_salt,
        }
    }
}

/// Wrapper struct for storing an encrypted data on disk.
///
/// The plaintext struct containing the data to encrypt is first serialized to CBOR, then encrypted using
/// [AES-GCM](https://en.wikipedia.org/wiki/Galois/Counter_Mode).
///
/// The resulting ciphertext is stored in `encrypted_payload`, and the AES-GCM
/// nonce used for encryption is stored in `nonce`.
///
/// Note: The term “IV” (Initialization Vector) used in AES-GCM — including in the linked Wikipedia page —
/// refers to the same value as the nonce. They are conceptually the same in this context.
///
/// This wrapper itself is then serialized to CBOR and written to disk.
#[derive(Serialize, Deserialize, Debug)]
pub struct EncryptedData {
    /// Nonce used for AES-GCM encryption (must match during decryption).
    nonce: EncryptionNonce,
    /// AES-GCM-encrypted CBOR-serialized plaintext struct data.
    encrypted_payload: Vec<u8>,
    /// Salt for the PBKDF2 key generation
    pbkdf2_salt: PBKDF2Salt,
}

/// Encrypts a serializable struct using AES-256-GCM encryption and CBOR serialization.
///
/// This function applies the following transformation pipeline:
/// `Struct -> serde_cbor::ser::to_vec(Struct) -> AES-GCM(encrypted_bytes) = encrypted_payload -> EncryptedData { encrypted_payload, nonce }`
///
///
/// The struct is first serialized into CBOR bytes, then encrypted using AES-GCM
/// with the key and nonce provided in [`KeyMaterial`]. The resulting ciphertext
/// is bundled with the nonce in an [`EncryptedData`] struct for storage.
///
/// The resulting `EncryptedData` can be serialized and stored to disk. To decrypt it later,
/// use [`decrypt_struct`].
pub fn encrypt_struct<T: Serialize>(
    plain_struct: T,
    enc_material: &KeyMaterial,
) -> Result<EncryptedData, EncryptError> {
    // Serialize wallet data to bytes.
    let packed_store = serde_cbor::ser::to_vec(&plain_struct)?;

    // Extract nonce and key for AES-GCM.
    let material_nonce = enc_material.nonce;
    let pbkdf2_salt = enc_material.pbkdf2_salt;
    let nonce = aes_gcm::Nonce::from_slice(&material_nonce);
    let key = Key::<Aes256Gcm>::from_slice(&enc_material.key);

    // Create AES-GCM cipher instance.
    let cipher = Aes256Gcm::new(key);

    // Encrypt the serialized wallet bytes.
    let encrypted_payload = cipher.encrypt(nonce, packed_store.as_ref())?;

    // Package encrypted data with nonce for storage.
    Ok(EncryptedData {
        nonce: material_nonce,
        encrypted_payload,
        pbkdf2_salt,
    })
}

/// Decrypts an [`EncryptedData`] struct and deserializes the inner struct.
///
/// This function reverses the encryption pipeline:
/// `EncryptedData -> AES-GCM Decryption -> CBOR -> Struct`
///
/// It uses the AES-256-GCM key and nonce in [`KeyMaterial`] to decrypt the
/// encrypted CBOR payload, then deserializes it into the original struct.
pub fn decrypt_struct<T: DeserializeOwned>(
    encrypted_struct: EncryptedData,
    enc_material: &KeyMaterial,
) -> Result<T, serde_cbor::Error> {
    // Deserialize the outer EncryptedWalletStore wrapper.

    let nonce_vec = encrypted_struct.nonce;

    // Reconstruct AES-GCM cipher from the provided key and stored nonce.
    let key = Key::<Aes256Gcm>::from_slice(&enc_material.key);
    let cipher = Aes256Gcm::new(key);
    let nonce = aes_gcm::Nonce::from_slice(&nonce_vec);

    // Decrypt the inner CBOR bytes.
    let plaintext_cbor = cipher
        .decrypt(nonce, encrypted_struct.encrypted_payload.as_ref())
        .expect("Error decrypting wallet, wrong passphrase?");

    // Deserialize the inner CBOR into the original type
    utill::deserialize_from_cbor::<T>(plaintext_cbor)
}
/// Loads a sensitive struct from a file, supporting both encrypted and plaintext formats.
///
/// This function tries to deserialize the file contents in two steps:
///
/// 1. **Unencrypted:** Attempts to deserialize the file directly as `T`.
/// 2. **Encrypted:** If that fails, attempts to deserialize as [`EncryptedData`],
///    then decrypts it using a user-supplied passphrase (prompted interactively).
///
/// The deserialization format (CBOR or JSON) is defined by the [`SerdeFormat`] trait
/// implementation passed via the type parameter `F`.
///
/// # Type Parameters
/// - `T`: The struct type to load.
/// - `F`: A type implementing [`SerdeFormat`] (`SerdeCbor` or `SerdeJson`).
pub fn load_sensitive_struct_interactive<T: DeserializeOwned, F: SerdeFormat>(
    path: &Path,
) -> (T, Option<KeyMaterial>) {
    let content = fs::read(path).unwrap_or_else(|_| panic!("Failed to read the file: {:?}", path));

    let (sensitive_struct, encryption_material) = match F::from_slice::<T>(&content) {
        Ok(unencrypted_struct) => (unencrypted_struct, None),
        Err(unencrypted_err) => match F::from_slice::<EncryptedData>(&content) {
            Ok(encrypted_struct) => {
                let encryption_password =
                    utill::prompt_password("Enter encryption passphrase: ".to_string())
                        .expect("Failed to read password");
                let enc_material = KeyMaterial::existing(
                    encryption_password,
                    encrypted_struct.nonce,
                    encrypted_struct.pbkdf2_salt,
                );

                let decrypted = decrypt_struct::<T>(encrypted_struct, &enc_material)
                    .unwrap_or_else(|err| panic!("Failed to decrypt file {:?}: {:?}", path, err));

                (decrypted, Some(enc_material))
            }
            Err(encrypted_err) => {
                panic!(
                    "Failed to deserialize file {:?}:\n- As unencrypted: {}\n- As encrypted: {}",
                    path, unencrypted_err, encrypted_err
                );
            }
        },
    };

    (sensitive_struct, encryption_material)
}

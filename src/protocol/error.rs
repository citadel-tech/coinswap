//! All Contract related errors.

use bitcoin::Amount;

/// Represents errors encountered during protocol operations.
///
/// This enum encapsulates various errors that can occur during protocol execution,
/// including cryptographic errors, mismatches in expected values, and general protocol violations.
#[derive(Debug)]
pub enum ProtocolError {
    /// Error related to Secp256k1 cryptographic operations.
    Secp(bitcoin::secp256k1::Error),
    /// Error in Bitcoin script handling.
    Script(bitcoin::blockdata::script::Error),
    /// Error converting from a byte slice to a hash type.
    Hash(bitcoin::hashes::FromSliceError),
    /// Error converting from a byte slice to a key type.
    Key(bitcoin::key::FromSliceError),
    /// Error related to calculating or validating Sighashes.
    Sighash(bitcoin::transaction::InputsIndexError),
    /// Error when an unexpected message is received.
    WrongMessage {
        /// The expected message type.
        expected: String,
        /// The received message type.
        received: String,
    },
    /// Error when the number of signatures does not match the expected count.
    WrongNumOfSigs {
        /// The expected number of signatures.
        expected: usize,
        /// The received number of signatures.
        received: usize,
    },
    /// Error when the number of contract transactions is incorrect.
    WrongNumOfContractTxs {
        /// The expected number of contract transactions.
        expected: usize,
        /// The received number of contract transactions.
        received: usize,
    },
    /// Error when the number of private keys is incorrect.
    WrongNumOfPrivkeys {
        /// The expected number of private keys.
        expected: usize,
        /// The received number of private keys.
        received: usize,
    },
    /// Error when the funding amount does not match the expected value.
    IncorrectFundingAmount {
        /// The expected funding amount.
        expected: Amount,
        /// The actual funding amount.
        found: Amount,
    },
    /// Error encountered when a non-segwit `script_pubkey` is used.
    ///
    /// The protocol only supports `V0_Segwit` transactions.
    ScriptPubkey(bitcoin::script::witness_program::Error),
    /// Represents an error with secp256k1::scalar conversion.
    ScalarOutOfRange(secp256k1::scalar::OutOfRangeError),
    /// General error not covered by other variants.
    General(&'static str),
    /// Represents an error related to Secp256k1 cryptographic operations.
    TaprootSecp(secp256k1::Error),
    ///Repesents an error in Taproot Script handling
    TaprootScript(bitcoin::taproot::TaprootBuilderError),
    /// Represents an error in Building a Taproot Tree
    TaprootBuilder(bitcoin::taproot::TaprootBuilder),
    /// Represents an error with Tweaking a PublicKey.
    MusigTweak(secp256k1::musig::InvalidTweakErr),
    /// Represents an error with computing a Taproot Signature from a slice.
    ///
    /// Generally occuring when computing Taproot Signature from a byte array of aggregated signatures.
    TaprootSigSlice(bitcoin::taproot::SigFromSliceError),
    /// Error related to calculating or validating Signature hash
    TaprootSighash(bitcoin::sighash::TaprootError),
}

impl From<bitcoin::script::witness_program::Error> for ProtocolError {
    fn from(value: bitcoin::script::witness_program::Error) -> Self {
        Self::ScriptPubkey(value)
    }
}

impl From<bitcoin::secp256k1::Error> for ProtocolError {
    fn from(value: bitcoin::secp256k1::Error) -> Self {
        Self::Secp(value)
    }
}

impl From<secp256k1::Error> for ProtocolError {
    fn from(value: secp256k1::Error) -> Self {
        Self::TaprootSecp(value)
    }
}

impl From<bitcoin::blockdata::script::Error> for ProtocolError {
    fn from(value: bitcoin::blockdata::script::Error) -> Self {
        Self::Script(value)
    }
}

impl From<bitcoin::hashes::FromSliceError> for ProtocolError {
    fn from(value: bitcoin::hashes::FromSliceError) -> Self {
        Self::Hash(value)
    }
}

impl From<bitcoin::key::FromSliceError> for ProtocolError {
    fn from(value: bitcoin::key::FromSliceError) -> Self {
        Self::Key(value)
    }
}

impl From<bitcoin::transaction::InputsIndexError> for ProtocolError {
    fn from(value: bitcoin::transaction::InputsIndexError) -> Self {
        Self::Sighash(value)
    }
}

impl From<bitcoin::taproot::TaprootBuilderError> for ProtocolError {
    fn from(value: bitcoin::taproot::TaprootBuilderError) -> Self {
        Self::TaprootScript(value)
    }
}
impl From<bitcoin::taproot::TaprootBuilder> for ProtocolError {
    fn from(value: bitcoin::taproot::TaprootBuilder) -> Self {
        Self::TaprootBuilder(value)
    }
}

impl From<bitcoin::sighash::TaprootError> for ProtocolError {
    fn from(value: bitcoin::sighash::TaprootError) -> Self {
        Self::TaprootSighash(value)
    }
}
impl From<secp256k1::scalar::OutOfRangeError> for ProtocolError {
    fn from(value: secp256k1::scalar::OutOfRangeError) -> Self {
        Self::ScalarOutOfRange(value)
    }
}
impl From<secp256k1::musig::InvalidTweakErr> for ProtocolError {
    fn from(value: secp256k1::musig::InvalidTweakErr) -> Self {
        Self::MusigTweak(value)
    }
}

impl From<bitcoin::taproot::SigFromSliceError> for ProtocolError {
    fn from(value: bitcoin::taproot::SigFromSliceError) -> Self {
        Self::TaprootSigSlice(value)
    }
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtocolError::Secp(e) => write!(f, "Secp256k1 error: {}", e),
            ProtocolError::Script(e) => write!(f, "Script error: {}", e),
            ProtocolError::Hash(e) => write!(f, "Hash conversion error: {}", e),
            ProtocolError::Key(e) => write!(f, "Key conversion error: {}", e),
            ProtocolError::Sighash(e) => write!(f, "Sighash error: {}", e),
            ProtocolError::WrongMessage { expected, received } => {
                write!(
                    f,
                    "Wrong message: expected {}, received {}",
                    expected, received
                )
            }
            ProtocolError::WrongNumOfSigs { expected, received } => {
                write!(
                    f,
                    "Wrong number of signatures: expected {}, received {}",
                    expected, received
                )
            }
            ProtocolError::WrongNumOfContractTxs { expected, received } => {
                write!(
                    f,
                    "Wrong number of contract txs: expected {}, received {}",
                    expected, received
                )
            }
            ProtocolError::WrongNumOfPrivkeys { expected, received } => {
                write!(
                    f,
                    "Wrong number of private keys: expected {}, received {}",
                    expected, received
                )
            }
            ProtocolError::IncorrectFundingAmount { expected, found } => {
                write!(
                    f,
                    "Incorrect funding amount: expected {}, found {}",
                    expected, found
                )
            }
            ProtocolError::ScriptPubkey(e) => write!(f, "Script pubkey error: {}", e),
            ProtocolError::ScalarOutOfRange(e) => write!(f, "Scalar out of range: {}", e),
            ProtocolError::General(msg) => write!(f, "{}", msg),
            ProtocolError::TaprootSecp(e) => write!(f, "Taproot secp256k1 error: {}", e),
            ProtocolError::TaprootScript(e) => write!(f, "Taproot script error: {}", e),
            ProtocolError::TaprootBuilder(e) => write!(f, "Taproot builder error: {:?}", e),
            ProtocolError::MusigTweak(e) => write!(f, "MuSig tweak error: {}", e),
            ProtocolError::TaprootSigSlice(e) => write!(f, "Taproot signature slice error: {}", e),
            ProtocolError::TaprootSighash(e) => write!(f, "Taproot sighash error: {}", e),
        }
    }
}

impl std::error::Error for ProtocolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ProtocolError::Secp(e) => Some(e),
            ProtocolError::Script(e) => Some(e),
            ProtocolError::Hash(e) => Some(e),
            ProtocolError::Key(e) => Some(e),
            ProtocolError::Sighash(e) => Some(e),
            ProtocolError::ScriptPubkey(e) => Some(e),
            ProtocolError::TaprootSecp(e) => Some(e),
            ProtocolError::TaprootScript(e) => Some(e),
            ProtocolError::TaprootSigSlice(e) => Some(e),
            ProtocolError::TaprootSighash(e) => Some(e),
            _ => None,
        }
    }
}

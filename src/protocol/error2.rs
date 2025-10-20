//! All taproot related errors

/// Represents errors encountered during taproot protocol operations.
#[derive(Debug)]
pub enum TaprootProtocolError {
    ///Repesents an error in Taproot Script handling
    Script(bitcoin::taproot::TaprootBuilderError),
    /// Represents an error in Building a Taproot Tree
    Builder(bitcoin::taproot::TaprootBuilder),
    /// General error not covered by other variants.
    General(&'static str),
    /// Represents an error related to Secp256k1 cryptographic operations.
    Secp(secp256k1::Error),
    /// Represents an error related to bitcoin::Secp256k1 cryptographic operations.
    BitcoinSecp(bitcoin::secp256k1::Error),
    /// Represents an error with secp256k1::scalar conversion.
    Scalar(secp256k1::scalar::OutOfRangeError),
    /// Represents an error with Tweaking a PublicKey.
    Tweak(secp256k1::musig::InvalidTweakErr),
    /// Represents an error with computing a Taproot Signature from a slice.
    ///
    /// Generally occuring when computing Taproot Signature from a byte array of aggregated signatures.
    SigSlice(bitcoin::taproot::SigFromSliceError),
    /// Error related to calculating or validating Signature hash
    Sighash(bitcoin::sighash::TaprootError),
}

impl From<bitcoin::taproot::TaprootBuilderError> for TaprootProtocolError {
    fn from(value: bitcoin::taproot::TaprootBuilderError) -> Self {
        Self::Script(value)
    }
}
impl From<bitcoin::taproot::TaprootBuilder> for TaprootProtocolError {
    fn from(value: bitcoin::taproot::TaprootBuilder) -> Self {
        Self::Builder(value)
    }
}
impl From<bitcoin::sighash::TaprootError> for TaprootProtocolError {
    fn from(value: bitcoin::sighash::TaprootError) -> Self {
        Self::Sighash(value)
    }
}
impl From<secp256k1::Error> for TaprootProtocolError {
    fn from(value: secp256k1::Error) -> Self {
        Self::Secp(value)
    }
}

impl From<bitcoin::secp256k1::Error> for TaprootProtocolError {
    fn from(value: bitcoin::secp256k1::Error) -> Self {
        Self::BitcoinSecp(value)
    }
}

impl From<secp256k1::scalar::OutOfRangeError> for TaprootProtocolError {
    fn from(value: secp256k1::scalar::OutOfRangeError) -> Self {
        Self::Scalar(value)
    }
}

impl From<secp256k1::musig::InvalidTweakErr> for TaprootProtocolError {
    fn from(value: secp256k1::musig::InvalidTweakErr) -> Self {
        Self::Tweak(value)
    }
}

impl From<bitcoin::taproot::SigFromSliceError> for TaprootProtocolError {
    fn from(value: bitcoin::taproot::SigFromSliceError) -> Self {
        Self::SigSlice(value)
    }
}

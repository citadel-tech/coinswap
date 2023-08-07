#[derive(Debug)]
pub enum WalletError {
    File(std::io::Error),
    Json(serde_json::Error),
    Rpc(bitcoincore_rpc::Error),
    Protocol(String),
    BIP32(bitcoin::util::bip32::Error),
    BIP39(bip39::Error),
}

impl From<std::io::Error> for WalletError {
    fn from(e: std::io::Error) -> Self {
        Self::File(e)
    }
}

impl From<serde_json::Error> for WalletError {
    fn from(e: serde_json::Error) -> Self {
        Self::File(e.into())
    }
}

impl From<bitcoincore_rpc::Error> for WalletError {
    fn from(value: bitcoincore_rpc::Error) -> Self {
        Self::Rpc(value)
    }
}

impl From<bitcoin::util::bip32::Error> for WalletError {
    fn from(value: bitcoin::util::bip32::Error) -> Self {
        Self::BIP32(value)
    }
}

impl From<bip39::Error> for WalletError {
    fn from(value: bip39::Error) -> Self {
        Self::BIP39(value)
    }
}

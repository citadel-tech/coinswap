use bitcoin::secp256k1;

#[derive(Debug)]
pub enum ContractError {
    Secp(secp256k1::Error),
    Protocol(&'static str),
    Script(bitcoin::blockdata::script::Error),
    Hash(bitcoin::hashes::Error),
    Key(bitcoin::key::Error),
    Sighash(bitcoin::sighash::Error),
    Addrs(bitcoin::address::Error),
}

impl From<secp256k1::Error> for ContractError {
    fn from(value: secp256k1::Error) -> Self {
        Self::Secp(value)
    }
}

impl From<bitcoin::blockdata::script::Error> for ContractError {
    fn from(value: bitcoin::blockdata::script::Error) -> Self {
        Self::Script(value)
    }
}

impl From<bitcoin::hashes::Error> for ContractError {
    fn from(value: bitcoin::hashes::Error) -> Self {
        Self::Hash(value)
    }
}

impl From<bitcoin::key::Error> for ContractError {
    fn from(value: bitcoin::key::Error) -> Self {
        Self::Key(value)
    }
}

impl From<bitcoin::sighash::Error> for ContractError {
    fn from(value: bitcoin::sighash::Error) -> Self {
        Self::Sighash(value)
    }
}

impl From<bitcoin::address::Error> for ContractError {
    fn from(value: bitcoin::address::Error) -> Self {
        Self::Addrs(value)
    }
}

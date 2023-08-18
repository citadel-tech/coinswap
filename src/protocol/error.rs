use bitcoin::secp256k1;

#[derive(Debug)]
pub enum ContractError {
    Keys(bitcoin::util::key::Error),
    Secp(secp256k1::Error),
    Protocol(&'static str),
    Script(bitcoin::blockdata::script::Error),
}

impl From<secp256k1::Error> for ContractError {
    fn from(value: secp256k1::Error) -> Self {
        Self::Secp(value)
    }
}

impl From<bitcoin::util::key::Error> for ContractError {
    fn from(value: bitcoin::util::key::Error) -> Self {
        Self::Keys(value)
    }
}

impl From<bitcoin::blockdata::script::Error> for ContractError {
    fn from(value: bitcoin::blockdata::script::Error) -> Self {
        Self::Script(value)
    }
}

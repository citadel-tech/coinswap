use crate::protocol::error::ContractError;

/// Includes all network related errors.
#[derive(Debug)]
pub enum NetError {
    IO(std::io::Error),
    Json(serde_json::Error),
    ReachedEOF,
    ConnectionTimedOut,
}

impl From<std::io::Error> for NetError {
    fn from(value: std::io::Error) -> Self {
        Self::IO(value)
    }
}

impl From<serde_json::Error> for NetError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

/// Includes all Protocol level errors.
#[derive(Debug)]
pub enum ProtocolError {
    WrongMessage { expected: String, received: String },
    WrongNumOfSigs { expected: usize, received: usize },
    WrongNumOfContractTxs { expected: usize, received: usize },
    WrongNumOfPrivkeys { expected: usize, received: usize },
    IncorrectFundingAmount { expected: u64, found: u64 },
    Contract(ContractError),
}

impl From<ContractError> for ProtocolError {
    fn from(value: ContractError) -> Self {
        Self::Contract(value)
    }
}

#[derive(Debug)]
pub enum WatcherError {
    ServerError,
    MempoolIndexerError,
    Shutdown,
    ParsingError,
    SendError,
    IOError(std::io::Error),
    RPCError(bitcoind::bitcoincore_rpc::Error),
    SerdeCbor(serde_cbor::Error),
    General(String),
}

impl From<std::io::Error> for WatcherError {
    fn from(value: std::io::Error) -> Self {
        WatcherError::IOError(value)
    }
}

impl From<bitcoind::bitcoincore_rpc::Error> for WatcherError {
    fn from(value: bitcoind::bitcoincore_rpc::Error) -> Self {
        WatcherError::RPCError(value)
    }
}

impl std::fmt::Display for WatcherError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl From<serde_cbor::Error> for WatcherError {
    fn from(value: serde_cbor::Error) -> Self {
        Self::SerdeCbor(value)
    }
}

impl WatcherError {
    pub fn io_error_kind(&self) -> Option<std::io::ErrorKind> {
        match self {
            WatcherError::IOError(e) => Some(e.kind()),
            _ => None,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            WatcherError::ServerError => "ServerError",
            WatcherError::MempoolIndexerError => "MempoolIndexerError",
            WatcherError::Shutdown => "Shutdown",
            WatcherError::ParsingError => "ParsingError",
            WatcherError::SendError => "SendError",
            WatcherError::IOError(_) => "IOError",
            WatcherError::RPCError(_) => "RPCError",
            WatcherError::SerdeCbor(_) => "SerdeCbor",
            WatcherError::General(_) => "General",
        }
    }
}

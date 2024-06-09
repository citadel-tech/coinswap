use std::{collections::HashMap, net::SocketAddr, path::PathBuf, thread, time::Duration};
use std::cell::Cell;

use nakamoto::{
    client::{
        chan::Receiver,
        Client,
        Config,
        Event,
        Handle as ClientHandle,
        handle::Handle,
    },
    net::poll::Waker,
};
use nakamoto::chain::Transaction;
use nakamoto::p2p::fsm::fees::FeeEstimate;

type Reactor = nakamoto::net::poll::Reactor<std::net::TcpStream>;

pub struct CbfBlockchain {
    receiver: Receiver<Event>,
    client_handle: ClientHandle<Waker>,
    timeout: Duration,
    fee_data: Cell<HashMap<u32, FeeEstimate>>,
    broadcasted_txs: Cell<Vec<Transaction>>,
    last_sync_height: Cell<u32>,
}

pub enum CbfSyncError {
    FilterHeaderRetrievalError,
    BlockFilterDownloadError,
    TransactionRetrievalError,
}

impl From<nakamoto::client::Error> for CbfSyncError {
    fn from(_: nakamoto::client::Error) -> Self {
        CbfSyncError::FilterHeaderRetrievalError
    }
}

impl CbfBlockchain {
    pub fn new(
        network: bitcoin::Network,
        datadir: Option<PathBuf>,
        peers: Vec<SocketAddr>,
    ) -> Result<Self, CbfSyncError> {
        let root = if let Some(dir) = datadir {
            dir
        } else {
            PathBuf::from(std::env::var("HOME").unwrap_or_default())
        };
        let cbf_client = Client::<Reactor>::new()?;
        let client_cfg = Config {
            network: network.into(),
            listen: vec![],
            root,
            ..Config::default()
        };

        let client_handle = cbf_client.handle();
        thread::spawn(move || {
            cbf_client.run(client_cfg).unwrap();
        });
        for peer in peers {
            client_handle
                .connect(peer)
                .map_err(nakamoto::client::Error::from)
                .map_err(CbfSyncError::from)?;
        }

        Ok(Self {
            receiver,
            client_handle,
            timeout: Duration::from_secs(60), // This is nakamoto default client timeout
            fee_data: Cell::new(HashMap::new()),
            broadcasted_txs: Cell::new(Vec::new()),
            last_sync_height: Cell::new(0u32),
        })
    }

    pub fn initialize_cbf_sync(&mut self) -> Result<(), CbfSyncError> {
        let last_sync_height = self.client_handle.get_tip();
        let (height, _) = last_sync_height?;
        Ok(())
    }

    //TO BE IMPLEMENTED
}
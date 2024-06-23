use std::{collections::HashMap, net::SocketAddr, path::PathBuf, thread, time::Duration};
use std::cell::Cell;
use bitcoin::Script;
use log::debug;
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
use crate::utill::get_taker_dir;

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
    NakamotoError(nakamoto::client::Error),
}

impl From<nakamoto::client::Error> for CbfSyncError {
    fn from(err: nakamoto::client::Error) -> Self {
        CbfSyncError::NakamotoError(err)
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
            get_taker_dir().join(("cbf"))
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
        let last_sync_height = self.client_handle.get_tip().map_err(nakamoto::client::Error::from)?;
        let (height, _) = last_sync_height?;
        self.last_sync_height.set(height);
        Ok(())
    }

    pub fn scan(&self, from: u32, scripts: Vec<Script>) {
        let _ = self.client_handle.rescan((from as u64).., scripts.into_iter());
    }

    fn add_fee_data(&self, height: u32, fee_estimate: FeeEstimate) {
        let mut data = self.fee_data.take();
        data.insert(height, fee_estimate);
        self.fee_data.set(data);
    }

    pub fn get_next_event(&self) -> Result<Event, CbfSyncError> {
        Ok(self.receiver.recv().map_err(|e| nakamoto::client::Error::from(nakamoto::client::handle::Error::from(e)))?)
    }

    pub fn process_events(&self) -> Result<(), CbfSyncError> {
        loop {
            match self.get_next_event()? {
                Event::BlockConnected { hash, height, .. } => {
                    debug!("Block connected: {} at height {}", hash, height);
                }
                Event::BlockDisconnected { hash, height, .. } => {
                    debug!("Block disconnected: {} at  height {}", hash, height);
                }
                Event::Synced { height, tip } => {
                    debug!("Sync complete up to {}/{}", height, tip);
                    if height == tip {
                        break;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}
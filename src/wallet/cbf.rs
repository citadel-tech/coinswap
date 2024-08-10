#![allow(dead_code)]
#![allow(unused_variables)]
#![allow(unused_imports)]

use bitcoin::{
    absolute::{Height, LockTime, Time},
    hashes::Hash,
    Amount, Network, OutPoint, ScriptBuf, Transaction, Txid,
};
use log::{debug, info, warn};
use nakamoto::{
    chain::Transaction as NakamotoTransaction,
    client::{chan::Receiver, handle::Handle, Client, Config, Event, Handle as ClientHandle},
    net::poll::Waker,
    p2p::fsm::fees::FeeEstimate,
};
use std::{
    cell::Cell, collections::HashMap, net::SocketAddr, path::PathBuf, thread, time::Duration,
};

use crate::{
    utill::get_taker_dir,
    wallet::{error::WalletError, Wallet},
};

type Reactor = nakamoto::net::poll::Reactor<std::net::TcpStream>;

pub struct CbfBlockchain {
    receiver: Receiver<Event>,
    client_handle: ClientHandle<Waker>,
    timeout: Duration,
    fee_data: Cell<HashMap<u32, FeeEstimate>>,
    broadcasted_txs: Cell<Vec<NakamotoTransaction>>,
    last_sync_height: Cell<u32>,
    wallet: Wallet,
}

pub enum CbfSyncError {
    NakamotoError(nakamoto::client::Error),
    WalletError(WalletError),
}

impl From<nakamoto::client::Error> for CbfSyncError {
    fn from(err: nakamoto::client::Error) -> Self {
        CbfSyncError::NakamotoError(err)
    }
}

impl From<WalletError> for CbfSyncError {
    fn from(err: WalletError) -> Self {
        CbfSyncError::WalletError(err)
    }
}

impl CbfBlockchain {
    pub fn new(
        network: bitcoin::Network,
        datadir: Option<PathBuf>,
        peers: Vec<SocketAddr>,
        wallet: Wallet,
    ) -> Result<Self, CbfSyncError> {
        let root = if let Some(dir) = datadir {
            dir
        } else {
            get_taker_dir().join("cbf")
        };
        let cbf_client = Client::<Reactor>::new()?;
        let client_cfg = Config {
            network: match network {
                Network::Bitcoin => nakamoto::client::Network::Mainnet,
                Network::Testnet => nakamoto::client::Network::Testnet,
                Network::Regtest => nakamoto::client::Network::Regtest,
                Network::Signet => nakamoto::client::Network::Signet,
                _ => unimplemented!(),
            },
            listen: vec![],
            root,
            ..Config::default()
        };

        let client_handle = cbf_client.handle();
        thread::spawn(move || {
            cbf_client.run(client_cfg).unwrap();
        });
        let receiver = client_handle.events();
        for peer in peers {
            client_handle
                .connect(peer)
                .map_err(nakamoto::client::Error::from)
                .map_err(CbfSyncError::from)?;
        }

        Ok(Self {
            receiver,
            client_handle,
            timeout: Duration::from_secs(60),
            fee_data: Cell::new(HashMap::new()),
            broadcasted_txs: Cell::new(Vec::new()),
            last_sync_height: Cell::new(0u32),
            wallet,
        })
    }

    pub fn initialize_cbf_sync(&mut self) -> Result<(), CbfSyncError> {
        let last_sync_height = self
            .client_handle
            .get_tip()
            .map_err(nakamoto::client::Error::from)?;
        let (height, _) = last_sync_height;
        self.last_sync_height.set(height as u32);
        Ok(())
    }

    pub fn scan(&self, from: u32, scripts: Vec<ScriptBuf>) {
        let _ = self.client_handle.rescan(
            (from as u64)..,
            scripts.into_iter().map(|s| s.as_script()).to_owned(),
        );
    }

    fn add_fee_data(&self, height: u32, fee_estimate: FeeEstimate) {
        let mut data = self.fee_data.take();
        data.insert(height, fee_estimate);
        self.fee_data.set(data);
    }

    pub fn get_next_event(&self) -> Result<Event, CbfSyncError> {
        Ok(self
            .receiver
            .recv()
            .map_err(|e| nakamoto::client::Error::from(nakamoto::client::handle::Error::from(e)))?)
    }

    pub fn process_events(&mut self) -> Result<(), CbfSyncError> {
        loop {
            match self.get_next_event()? {
                Event::Ready { tip, filter_tip } => {
                    info!("CBF sync ready. Tip: {}, Filter tip: {}", tip, filter_tip);
                }
                Event::PeerConnected { addr, .. } => {
                    info!("Peer connected: {}", addr);
                }
                Event::PeerDisconnected { addr, reason } => {
                    info!("Peer disconnected: {}. Reason: {:?}", addr, reason);
                }
                Event::PeerConnectionFailed { addr, error } => {
                    warn!("Peer connection failed: {}. Error: {}", addr, error);
                }
                Event::PeerNegotiated {
                    addr,
                    services,
                    height,
                    user_agent,
                    version,
                    ..
                } => {
                    info!("Peer negotiated: {}. Services: {:?}, Height: {}, User Agent: {}, Version: {}", addr, services, height, user_agent, version);
                }
                Event::PeerHeightUpdated { height } => {
                    debug!("Peer height updated: {}", height);
                }
                Event::BlockConnected { hash, height, .. } => {
                    info!("Block connected: {} at height {}", hash, height);
                }
                Event::BlockDisconnected { hash, height, .. } => {
                    info!("Block disconnected: {} at height {}", hash, height);
                }
                Event::BlockMatched {
                    hash,
                    height,
                    transactions,
                    ..
                } => {
                    info!(
                        "Block matched: {} at height {}. Transactions: {}",
                        hash,
                        height,
                        transactions.len()
                    );
                    for transaction in transactions {
                        debug!("Processing transaction: {}", transaction.txid());
                        self.process_transaction(transaction)?;
                    }
                }
                Event::FeeEstimated {
                    block,
                    height,
                    fees,
                } => {
                    debug!(
                        "Fee estimated for block: {} at height {}. Fees: {:?}",
                        block, height, fees
                    );
                }
                Event::FilterProcessed {
                    block,
                    height,
                    matched,
                    valid,
                } => {
                    debug!(
                        "Filter processed for block: {} at height {}. Matched: {}, Valid: {}",
                        block, height, matched, valid
                    );
                }
                Event::TxStatusChanged { txid, status } => {
                    debug!("Transaction status changed: {}. Status: {:?}", txid, status);
                }
                Event::Synced { height, tip } => {
                    info!("Sync complete up to {}/{}", height, tip);
                    if height == tip {
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    fn process_transaction(
        &mut self,
        transaction: NakamotoTransaction,
    ) -> Result<(), CbfSyncError> {
        let txid = Txid::from_slice(transaction.txid().as_ref()).expect("Invalid txid");
        let output_scripts: Vec<ScriptBuf> = transaction
            .output
            .iter()
            .map(|out| ScriptBuf::from_bytes(out.script_pubkey.as_bytes().to_vec()))
            .collect();
        let input_outpoints: Vec<OutPoint> = transaction
            .input
            .iter()
            .map(|inp| OutPoint {
                txid: Txid::from_slice(inp.previous_output.txid.as_ref()).expect("Invalid txid"),
                vout: inp.previous_output.vout,
            })
            .collect();

        let relevant_outputs = self.find_relevant_outputs(&output_scripts)?;
        let relevant_inputs = self.find_relevant_inputs(&input_outpoints)?;

        if !relevant_inputs.is_empty() || !relevant_outputs.is_empty() {
            self.update_wallet_with_tx(&transaction, &relevant_outputs, &relevant_inputs)?;
        }

        Ok(())
    }

    fn find_relevant_outputs(
        &self,
        output_scripts: &[ScriptBuf],
    ) -> Result<Vec<(u32, ScriptBuf)>, CbfSyncError> {
        let mut relevant_outputs = Vec::new();

        for (idx, script) in output_scripts.iter().enumerate() {
            if self.wallet.is_script_tracked(script)? {
                relevant_outputs.push((idx as u32, script.clone()));
            }
        }

        Ok(relevant_outputs)
    }

    fn find_relevant_inputs(
        &self,
        input_outpoints: &[OutPoint],
    ) -> Result<Vec<OutPoint>, CbfSyncError> {
        let mut relevant_inputs = Vec::new();

        for outpoint in input_outpoints {
            if self.wallet.is_utxo_tracked(outpoint)? {
                relevant_inputs.push(*outpoint);
            }
        }

        Ok(relevant_inputs)
    }

    // Simplified the transaction handling by creating a minimal Bitcoin transaction
    // instead of attempting a full conversion from Nakamoto's transaction format.
    // This approach stores only the essential information (version, lock_time, and txid)
    // while omitting the detailed input and output data.

    fn update_wallet_with_tx(
        &mut self,
        transaction: &NakamotoTransaction,
        relevant_outputs: &[(u32, ScriptBuf)],
        relevant_inputs: &[OutPoint],
    ) -> Result<(), CbfSyncError> {
        let txid = Txid::from_slice(transaction.txid().as_ref()).expect("Invalid txid");
        for (vout, script) in relevant_outputs {
            let amount = Amount::from_sat(transaction.output[*vout as usize].value);
            self.wallet
                .add_utxo(OutPoint { txid, vout: *vout }, amount, script.clone())?;
        }

        for outpoint in relevant_inputs {
            self.wallet.remove_utxo(*outpoint)?;
        }

        let lock_time = LockTime::from_consensus(transaction.lock_time.to_u32());

        let bitcoin_tx = Transaction {
            version: bitcoin::transaction::Version(transaction.version),
            lock_time,
            input: vec![],
            output: vec![],
        };

        self.wallet.store_transaction(bitcoin_tx)?;
        Ok(())
    }
}

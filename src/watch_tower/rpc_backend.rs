//! Watchtower RPC backend: querying bitcoind, scanning mempool, and running discovery.

use std::{
    sync::{Arc, Mutex, atomic::{AtomicU64, Ordering}},
    thread,
    time::Duration,
};

use bitcoincore_rpc::{
    bitcoin::{Block, BlockHash, Transaction, Txid},
    json::GetBlockchainInfoResult,
    Client, RpcApi,
};
use bitcoind::bitcoincore_rpc;

use crate::{
    wallet::RPCConfig,
    watch_tower::{
        registry_storage::{Checkpoint, FileRegistry},
        utils::{process_fidelity, process_transaction},
        watcher_error::WatcherError,
    },
};

const SIX_MONTHS_SECS: u64 = 6 * 30 * 24 * 60 * 60;

/// Lightweight wrapper around bitcoind RPC calls used by the watchtower.
pub struct BitcoinRpc {
    client: Client,
}

impl BitcoinRpc {
    /// Constructs a new RPC wrapper using the provided configuration.
    pub fn new(rpc_config: RPCConfig) -> Result<Self, WatcherError> {
        let client = Client::new(&rpc_config.url, rpc_config.auth)?;
        Ok(Self { client })
    }

    /// Wraps an existing bitcoind RPC client.
    pub fn new_client(client: Client) -> Self {
        Self { client }
    }

    /// Get txids of all transactions in the mempool.
    pub fn get_raw_mempool(&self) -> Result<Vec<Txid>, WatcherError> {
        let raw_mempool = self.client.get_raw_mempool()?;
        Ok(raw_mempool)
    }

    /// Fetches a full transaction by txid.
    pub fn get_raw_tx(&self, txid: &Txid) -> Result<Transaction, WatcherError> {
        let tx = self.client.get_raw_transaction(txid, None)?;
        Ok(tx)
    }

    /// Returns chain metadata.
    pub fn get_blockchain_info(&self) -> Result<GetBlockchainInfoResult, WatcherError> {
        let blockchain_info = self.client.get_blockchain_info()?;
        Ok(blockchain_info)
    }

    /// Retrieves the block hash at a given height.
    pub fn get_block_hash(&self, height: u64) -> Result<BlockHash, WatcherError> {
        let block_hash = self.client.get_block_hash(height)?;
        Ok(block_hash)
    }

    /// Fetches a block by hash.
    pub fn get_block_count(&self) -> Result<u64, WatcherError> {
        let height = self.client.get_block_count()?;
        Ok(height)
    }

    /// Fetches a block by hash.
    pub fn get_block(&self, hash: BlockHash) -> Result<Block, WatcherError> {
        let block = self.client.get_block(&hash)?;
        Ok(block)
    }

    /// Processes transactions in the mempool and updates registry.
    pub fn process_mempool(&mut self, registry: &mut FileRegistry) -> Result<(), WatcherError> {
        let txids = self.get_raw_mempool()?;
        for txid in &txids {
            let tx = self.get_raw_tx(txid)?;
            process_transaction(&tx, registry, false);
        }
        Ok(())
    }

    /// Discovers maker fidelity bonds by scanning historical blocks.
    pub fn run_discovery(self, registry: FileRegistry) -> Result<(), WatcherError> {
        log::info!("Starting with market discovery");
        let scanned_blocks = Arc::new(AtomicU64::new(0));

        let blockchain_info = self.get_blockchain_info()?;
        let block_hash = blockchain_info.best_block_hash;
        let tip_height = blockchain_info.blocks;
        let now = blockchain_info.median_time;
        let cutoff_time = now - SIX_MONTHS_SECS;

        let checkpoint = registry.load_checkpoint();
        let lowest_scanned = checkpoint.map(|c| c.height).unwrap_or(1);

        let threads = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);

        log::info!(
            "Running discovery with {} threads (heights {}..={})",
            threads,
            lowest_scanned,
            tip_height
        );

        let registry = Arc::new(Mutex::new(registry));
        let this = Arc::new(self);

        let mut handles = Vec::with_capacity(threads);

        for thread_id in 0..threads {
            let scanned_blocks = Arc::clone(&scanned_blocks);
            let registry = Arc::clone(&registry);
            let this = Arc::clone(&this);

            let handle = thread::spawn(move || {
                let mut local_found = 0;

                let mut height = tip_height as i64 - thread_id as i64;
                let step = threads as i64;

                while height >= lowest_scanned as i64 {
                    let h = height as u64;

                    let block_hash = match this.get_block_hash(h) {
                        Ok(bh) => bh,
                        Err(e) => {
                            log::warn!("Failed to get block hash at height {}: {}", h, e);
                            std::thread::sleep(Duration::from_millis(200));
                            continue;
                        }
                    };

                    let block = match this.get_block(block_hash) {
                        Ok(b) => b,
                        Err(e) => {
                            log::warn!("Failed to get block at height {}: {}", h, e);
                            std::thread::sleep(Duration::from_millis(200));
                            continue;
                        }
                    };

                    let prev = scanned_blocks.fetch_add(1, Ordering::Relaxed) + 1;

                    if prev % 100 == 0 {
                        log::info!(
                            "Market discovery progress: scanned {} blocks (tip={}, cutoff_height~{})",
                            prev,
                            tip_height,
                            lowest_scanned,
                        );
                    }

                    if block.header.time < cutoff_time as u32 {
                        break;
                    }

                    for tx in block.txdata {
                        if let Some(fidelity_announcement) = process_fidelity(&tx) {
                            local_found += 1;
                            log::info!("Fidelity found: {fidelity_announcement:#?}");
                            registry
                                .lock()
                                .unwrap()
                                .insert_fidelity(tx.compute_txid(), fidelity_announcement);
                        }
                    }

                    height -= step;
                }

                local_found
            });

            handles.push(handle);
        }

        let mut makers_found = 0;
        for handle in handles {
            makers_found += handle.join().expect("discovery thread panicked");
        }

        let checkpoint = registry.lock().unwrap().load_checkpoint();
        let lowest_scanned = checkpoint.map(|c| c.height).unwrap_or(1);

        if lowest_scanned < tip_height {
            log::info!("Saved checkpoint after scan completion height: {tip_height}.");
            registry.lock().unwrap().save_checkpoint(Checkpoint {
                height: tip_height,
                hash: block_hash,
            });
        }

        let total_scanned = scanned_blocks.load(Ordering::Relaxed);

        log::info!(
            "Market discovery completed: scanned {} blocks, found {} makers",
            total_scanned,
            makers_found
        );


        Ok(())
    }
}

impl From<Client> for BitcoinRpc {
    fn from(value: Client) -> Self {
        BitcoinRpc { client: value }
    }
}

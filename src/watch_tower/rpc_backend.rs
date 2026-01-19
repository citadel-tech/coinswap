//! Watchtower RPC backend: querying bitcoind, scanning mempool, and running discovery.

use bitcoincore_rpc::{
    bitcoin::{Block, BlockHash, Transaction, Txid},
    json::GetBlockchainInfoResult,
    Client, RpcApi,
};
use bitcoind::bitcoincore_rpc;

use crate::{
    wallet::RPCConfig,
    watch_tower::{
        registry_storage::FileRegistry, utils::process_transaction, watcher_error::WatcherError,
    },
};

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
}

impl From<Client> for BitcoinRpc {
    fn from(value: Client) -> Self {
        BitcoinRpc { client: value }
    }
}

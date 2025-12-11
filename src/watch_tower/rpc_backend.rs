//! Watchtower RPC backend: querying bitcoind, scanning mempool, and running discovery.

use bitcoin::Network;
use bitcoincore_rpc::{
    bitcoin::{Block, BlockHash, Transaction, Txid},
    json::GetBlockchainInfoResult,
    Client, RpcApi,
};
use bitcoind::bitcoincore_rpc;

use crate::{
    wallet::RPCConfig,
    watch_tower::{
        constants::{BITCOIN, REGTEST, SIGNET, TESTNET, TESTNET4},
        registry_storage::FileRegistry,
        utils::{process_fidelity, process_transaction},
        watcher_error::WatcherError,
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
    pub fn run_discovery(self, mut registry: FileRegistry) -> Result<(), WatcherError> {
        log::info!("Starting with market discovery");
        let blockchain_info = self.get_blockchain_info()?;
        let coinswap_height = match blockchain_info.chain {
            Network::Bitcoin => BITCOIN,
            Network::Regtest => REGTEST,
            Network::Signet => SIGNET,
            Network::Testnet => TESTNET,
            Network::Testnet4 => TESTNET4,
        };
        let last_tip = registry
            .load_checkpoint()
            .map(|checkpoint| checkpoint.height)
            .unwrap_or(coinswap_height);
        let tip_height = blockchain_info.blocks + 1;
        let total_blocks = tip_height.saturating_sub(last_tip);
        log::info!(
            "Scanning {} blocks for fidelity bonds (height {} to {})",
            total_blocks,
            last_tip,
            tip_height.saturating_sub(1)
        );
        let mut makers_found = 0;
        for (i, height) in (last_tip..tip_height).enumerate() {
            if total_blocks > 100 {
                log::info!(
                    "Discovery progress: {}/{} blocks scanned ({:.1}%)",
                    i + 1,
                    total_blocks,
                    ((i + 1) as f64 / total_blocks as f64) * 100.0
                );
            }
            let block_hash = self.get_block_hash(height)?;
            let block = self.get_block(block_hash)?;
            for tx in block.txdata {
                let onion_address = process_fidelity(&tx);
                if let Some(onion_address) = onion_address {
                    makers_found += 1;
                    log::info!("Maker found in the market: {:?}", onion_address);
                    registry.insert_fidelity(tx.compute_txid(), onion_address);
                }
            }
        }
        log::info!(
            "Market discovery completed: scanned {} blocks, found {} makers",
            total_blocks,
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

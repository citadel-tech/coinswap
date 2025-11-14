use bitcoincore_rpc::{
    bitcoin::{Block, BlockHash, Transaction, Txid},
    json::GetBlockchainInfoResult,
    Client, RpcApi,
};
use bitcoind::bitcoincore_rpc;
use log::info;

use crate::{
    wallet::RPCConfig,
    watch_tower::{
        registry_storage::FileRegistry,
        utils::{process_fidelity, process_transaction},
        watcher_error::WatcherError,
    },
};

pub struct BitcoinRpc {
    client: Client,
}

impl BitcoinRpc {
    pub fn new(rpc_config: RPCConfig) -> Result<Self, WatcherError> {
        let client = Client::new(&rpc_config.url, rpc_config.auth)?;
        Ok(Self { client })
    }

    pub fn new_client(client: Client) -> Self {
        Self { client }
    }

    pub fn get_raw_mempool(&self) -> Result<Vec<Txid>, WatcherError> {
        let raw_mempool = self.client.get_raw_mempool()?;
        Ok(raw_mempool)
    }

    pub fn get_raw_tx(&self, txid: &Txid) -> Result<Transaction, WatcherError> {
        let tx = self.client.get_raw_transaction(txid, None)?;
        Ok(tx)
    }

    pub fn get_blockchain_info(&self) -> Result<GetBlockchainInfoResult, WatcherError> {
        let blockchain_info = self.client.get_blockchain_info()?;
        Ok(blockchain_info)
    }

    pub fn get_block_hash(&self, height: u64) -> Result<BlockHash, WatcherError> {
        let block_hash = self.client.get_block_hash(height)?;
        Ok(block_hash)
    }

    pub fn get_block(&self, hash: BlockHash) -> Result<Block, WatcherError> {
        let block = self.client.get_block(&hash)?;
        Ok(block)
    }

    pub fn process_mempool(&mut self, registry: &mut FileRegistry) -> Result<(), WatcherError> {
        let txids = self.get_raw_mempool().unwrap();
        for txid in &txids {
            let tx = self.get_raw_tx(txid)?;
            process_transaction(&tx, registry, false);
        }
        Ok(())
    }

    pub fn run_recovery(&mut self, registry: &mut FileRegistry) -> Result<(), WatcherError> {
        let last_tip = registry
            .load_checkpoint()
            .map(|checkpoint| checkpoint.height)
            .unwrap_or(1);
        let blockchain_info = self.get_blockchain_info()?;
        let tip_height = blockchain_info.blocks + 1;
        for height in last_tip..tip_height {
            let block_hash = self.get_block_hash(height)?;
            let block = self.get_block(block_hash)?;
            for tx in block.txdata {
                let onion_address = process_fidelity(&tx);
                if let Some(onion_address) = onion_address {
                    info!("New address found: {:?}", onion_address);
                    registry.insert_fidelity(tx.compute_txid(), onion_address);
                }
            }
        }
        Ok(())
    }
}

impl From<Client> for BitcoinRpc {
    fn from(value: Client) -> Self {
        BitcoinRpc { client: value }
    }
}

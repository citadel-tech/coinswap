use std::{collections::HashMap, path::PathBuf};

use bitcoin::{BlockHash, OutPoint, Transaction, Txid};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchRequest {
    pub outpoint: OutPoint,
    pub in_block: bool,
    pub spent_tx: Option<Transaction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fidelity {
    pub txid: Txid,
    pub onion_address: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub height: u64,
    pub hash: BlockHash,
}

#[derive(Serialize, Deserialize, Default)]
struct RegistryData {
    watches: HashMap<OutPoint, WatchRequest>,
    fidelity: Vec<Fidelity>,
    checkpoint: Option<Checkpoint>,
}

pub struct FileRegistry {
    path: PathBuf,
    data: RegistryData,
}

impl FileRegistry {
    pub fn load<P: Into<PathBuf>>(path: P) -> Self {
        let path = path.into();
        let data = if path.exists() {
            let bytes = std::fs::read(&path).expect("read registry");
            serde_cbor::from_slice(&bytes).unwrap_or_default()
        } else {
            RegistryData::default()
        };
        Self { path, data }
    }

    fn flush(&self) {
        let tmp = self.path.with_extension("tmp");
        let bytes = serde_cbor::to_vec(&self.data).expect("serialize");
        std::fs::write(&tmp, bytes).expect("write tmp");
        std::fs::rename(tmp, &self.path).expect("atomic replace");
    }
}

impl FileRegistry {
    pub fn upsert_watch(&mut self, req: &WatchRequest) {
        self.data.watches.insert(req.outpoint, req.clone());
        self.flush();
    }

    pub fn remove_watch(&mut self, outpoint: OutPoint) {
        self.data.watches.remove(&outpoint);
        self.flush();
    }

    pub fn list_watches(&self) -> Vec<WatchRequest> {
        self.data.watches.values().cloned().collect()
    }

    pub fn list_fidelity(&self) -> Vec<Fidelity> {
        self.data.fidelity.clone()
    }

    pub fn insert_fidelity(&mut self, txid: Txid, onion_address: String) {
        let fidelity = Fidelity {
            txid,
            onion_address,
        };
        self.data.fidelity.push(fidelity);
    }

    pub fn remove_fidelity(&mut self, txid: Txid) {
        self.data.fidelity.retain(|f| f.txid != txid);
    }

    pub fn save_checkpoint(&mut self, cp: Checkpoint) {
        self.data.checkpoint = Some(cp);
        self.flush();
    }

    pub fn load_checkpoint(&self) -> Option<Checkpoint> {
        self.data.checkpoint.clone()
    }
}

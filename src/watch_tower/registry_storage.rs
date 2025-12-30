//! File-backed registry for watch requests, fidelity bonds, and chain checkpoints.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use bitcoin::{BlockHash, OutPoint, Transaction, Txid};
use serde::{Deserialize, Serialize};

use crate::watch_tower::utils::FidelityAnnouncement;

/// Represents a UTXO being watched and records when it gets spent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchRequest {
    /// UTXO being watched.
    pub outpoint: OutPoint,
    /// Whether the spend was seen in a block (`true`) or only in the mempool.
    pub in_block: bool,
    /// Optional full transaction that spent the outpoint.
    pub spent_tx: Option<Transaction>,
}

/// Fidelity records used to discover Makers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct Fidelity {
    /// Transaction ID of the maker's fidelity bond.
    pub txid: Txid,
    /// Maker's advertised onion address.
    pub onion_address: String,
    /// Fidelity expiry height used later for pruning.
    pub expire_height: u32,
}

/// Last processed chain tip so scanning can resume efficiently.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Checkpoint {
    /// Block height recorded.
    pub height: u64,
    /// Block hash recorded.
    pub hash: BlockHash,
}

#[derive(Serialize, Deserialize, Default)]
struct RegistryData {
    watches: HashMap<OutPoint, WatchRequest>,
    fidelity: HashSet<Fidelity>,
    checkpoint: Option<Checkpoint>,
}

/// Registry used by the watcher.
#[derive(Clone)]
pub struct FileRegistry {
    path: PathBuf,
    data: Arc<Mutex<RegistryData>>,
}

impl FileRegistry {
    /// Loads registry data from disk, creating the file and parent directories if missing.
    pub fn load<P: Into<PathBuf>>(path: P) -> Self {
        let path = path.into();
        let data = if path.exists() {
            match std::fs::read(&path) {
                Ok(bytes) => Arc::new(Mutex::new(
                    serde_cbor::from_slice(&bytes).unwrap_or_default(),
                )),
                Err(e) => {
                    log::error!("Failed to read registry file {:?}: {}", path, e);
                    Arc::new(Mutex::new(RegistryData::default()))
                }
            }
        } else {
            let data = Arc::new(Mutex::new(RegistryData::default()));
            if let Some(parent) = path.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    log::error!("Failed to create registry directory {:?}: {}", parent, e);
                    return Self { path, data };
                }
            }

            match serde_cbor::to_vec(&RegistryData::default()) {
                Ok(bytes) => {
                    if let Err(e) = std::fs::write(&path, bytes) {
                        log::error!("Failed to write initial registry file {:?}: {}", path, e);
                    }
                }
                Err(e) => {
                    log::error!("Failed to serialize initial registry data: {}", e);
                }
            }
            data
        };

        Self { path, data }
    }

    /// Flushes the in-memory registry to the persistent CBOR file on disk.
    fn flush(&self) {
        // Ensure parent directory exists
        if let Some(parent) = self.path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                log::error!("Failed to create registry directory {:?}: {}", parent, e);
                return;
            }
        }

        let tmp = self.path.with_extension("tmp");
        if let Ok(data) = self.data.lock() {
            let bytes = match serde_cbor::to_vec(&*data) {
                Ok(b) => b,
                Err(e) => {
                    log::error!("Failed to serialize registry data: {}", e);
                    return;
                }
            };

            if let Err(e) = std::fs::write(&tmp, &bytes) {
                log::error!("Failed to write tmp registry file {:?}: {}", tmp, e);
                return;
            }
            if let Err(e) = std::fs::rename(&tmp, &self.path) {
                log::error!(
                    "Failed to rename registry file {:?} -> {:?}: {}",
                    tmp,
                    self.path,
                    e
                );
            }
        }
    }
}

impl FileRegistry {
    /// Inserts a watch request and flushes the registry to disk.
    pub fn upsert_watch(&mut self, req: &WatchRequest) {
        self.with_data(|data| data.watches.insert(req.outpoint, req.clone()));
        self.flush();
    }

    /// Removes a watch request for the given outpoint and flushes the registry to disk.
    pub fn remove_watch(&mut self, outpoint: OutPoint) {
        self.with_data(|data| data.watches.remove(&outpoint));
        self.flush();
    }

    /// Returns all current watch requests.
    pub fn list_watches(&self) -> Vec<WatchRequest> {
        self.with_data(|data| data.watches.values().cloned().collect())
    }

    /// Returns all stored maker fidelity records.
    pub fn list_fidelity(&self, height: u32) -> HashSet<Fidelity> {
        self.with_data(|data| {
            data.fidelity = data
                .fidelity
                .iter()
                .filter(|v| v.expire_height > height)
                .cloned()
                .collect();
            data.fidelity.clone()
        })
    }

    /// Inserts a new fidelity record.
    pub fn insert_fidelity(
        &mut self,
        txid: Txid,
        fidelity_announcement: FidelityAnnouncement,
    ) -> bool {
        let fidelity = Fidelity {
            txid,
            onion_address: fidelity_announcement.onion,
            expire_height: fidelity_announcement.expires_at_height,
        };
        let is_in = self.with_data(|data| data.fidelity.insert(fidelity));
        self.flush();
        is_in
    }

    /// Removes fidelity records matching the given txid.
    pub fn remove_fidelity(&mut self, txid: Txid) {
        self.with_data(|data| data.fidelity.retain(|f| f.txid != txid));
        self.flush();
    }

    /// Persists the latest processed checkpoint to disk.
    pub fn save_checkpoint(&mut self, cp: Checkpoint) {
        self.with_data(|data| data.checkpoint = Some(cp));
        self.flush();
    }

    /// Loads the most recently saved checkpoint, if any.
    pub fn load_checkpoint(&self) -> Option<Checkpoint> {
        self.data.lock().unwrap().checkpoint.clone()
    }

    fn with_data<F, T>(&self, f: F) -> T
    where
        F: FnOnce(&mut RegistryData) -> T,
    {
        let mut data = self.data.lock().unwrap();
        f(&mut data)
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;
    use bitcoin::{BlockHash, OutPoint, Txid};
    use bitcoind::tempfile::TempDir;

    fn dummy_txid(_n: u8) -> Txid {
        Txid::from_str("a6eab3c14ab5272a58a5ba91505ba1a4b6d7a3a9fcbd187b6cd99a7b6d548cb7").unwrap()
    }

    fn dummy_outpoint(n: u8) -> OutPoint {
        OutPoint {
            txid: dummy_txid(n),
            vout: n as u32,
        }
    }

    fn dummy_checkpoint(n: u64) -> Checkpoint {
        Checkpoint {
            height: n,
            hash: BlockHash::from_str(
                "0000000000000000000000000000000000000000000000000000000000000000",
            )
            .unwrap(),
        }
    }

    #[test]
    fn test_load_creates_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("registry.cbor");

        assert!(!path.exists());
        let _reg = FileRegistry::load(&path);
        assert!(path.exists());
    }

    #[test]
    fn test_watch_upsert_and_reload() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("reg.cbor");

        let mut reg = FileRegistry::load(&path);

        let outpoint = dummy_outpoint(1);
        let req = WatchRequest {
            outpoint,
            in_block: false,
            spent_tx: None,
        };
        reg.upsert_watch(&req);

        let reg2 = FileRegistry::load(&path);
        let watches = reg2.list_watches();

        assert_eq!(watches.len(), 1);
        assert_eq!(watches[0].outpoint, outpoint);
    }

    #[test]
    fn test_watch_remove_and_reload() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("reg.cbor");

        let mut reg = FileRegistry::load(&path);
        let outpoint = dummy_outpoint(2);

        let req = WatchRequest {
            outpoint,
            in_block: true,
            spent_tx: None,
        };
        reg.upsert_watch(&req);
        reg.remove_watch(outpoint);

        let reg2 = FileRegistry::load(&path);
        assert!(reg2.list_watches().is_empty());
    }

    #[test]
    fn test_fidelity_insert_and_remove() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("reg.cbor");

        let mut reg = FileRegistry::load(&path);

        let txid1 = dummy_txid(1);
        let txid2 = dummy_txid(2);

        let fidelity_announcement_1 = FidelityAnnouncement {
            onion: "abc.onion".to_string(),
            expires_at_height: 212,
        };

        let fidelity_announcement_2 = FidelityAnnouncement {
            onion: "def.onion".to_string(),
            expires_at_height: 232,
        };

        reg.insert_fidelity(txid1, fidelity_announcement_1);
        reg.insert_fidelity(txid2, fidelity_announcement_2);

        let list = reg.list_fidelity(0);
        assert_eq!(list.len(), 2);

        reg.remove_fidelity(txid1);

        let list2 = reg.list_fidelity(0);
        assert_eq!(list2.len(), 0);
    }

    #[test]
    fn test_checkpoint_save_and_reload() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("reg.cbor");

        let mut reg = FileRegistry::load(&path);

        let cp = dummy_checkpoint(100);
        reg.save_checkpoint(cp.clone());

        let reg2 = FileRegistry::load(&path);

        assert_eq!(reg2.load_checkpoint().unwrap(), cp);
    }
}

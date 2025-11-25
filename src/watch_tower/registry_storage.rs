use std::{collections::HashMap, fs, path::PathBuf};

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
            let data = RegistryData::default();
            _ = std::fs::create_dir_all(path.parent().expect("Path should NOT be root!"));
            _ = fs::File::create(&path);
            let bytes = serde_cbor::to_vec(&data).expect("serialize");
            std::fs::write(&path, bytes).expect("Write to path");
            data
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

        reg.insert_fidelity(txid1, "abc.onion".into());
        reg.insert_fidelity(txid2, "def.onion".into());

        let list = reg.list_fidelity();
        assert_eq!(list.len(), 2);

        reg.remove_fidelity(txid1);

        let list2 = reg.list_fidelity();
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

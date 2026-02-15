use bitcoin::{OutPoint, Transaction};
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    str::FromStr,
};

use crate::wallet::WalletError;

pub(crate) const DEFAULT_UTXO_DENY_LIST_FILE: &str = "utxo-deny-list.txt";

pub(crate) fn resolve_deny_list_path(base_dir: &Path, configured_path: &str) -> PathBuf {
    let configured = configured_path.trim().trim_matches('"');
    if configured.is_empty() {
        return base_dir.join(DEFAULT_UTXO_DENY_LIST_FILE);
    }

    let path = PathBuf::from(configured);
    if path.is_absolute() {
        path
    } else {
        base_dir.join(path)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct UtxoDenyList {
    path: PathBuf,
    denied_outpoints: HashSet<OutPoint>,
}

impl UtxoDenyList {
    pub(crate) fn load(path: PathBuf) -> Result<Self, WalletError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        if !path.exists() {
            File::create(&path)?;
        }

        let denied_outpoints = Self::read_outpoints_from_file(&path)?;
        Ok(Self {
            path,
            denied_outpoints,
        })
    }

    pub(crate) fn list(&self) -> Vec<OutPoint> {
        let mut outpoints = self.denied_outpoints.iter().copied().collect::<Vec<_>>();
        outpoints.sort_by_key(|op| op.to_string());
        outpoints
    }

    #[allow(dead_code)]
    pub(crate) fn len(&self) -> usize {
        self.denied_outpoints.len()
    }

    #[allow(dead_code)]
    pub(crate) fn is_empty(&self) -> bool {
        self.denied_outpoints.is_empty()
    }

    pub(crate) fn contains(&self, outpoint: &OutPoint) -> bool {
        self.denied_outpoints.contains(outpoint)
    }

    pub(crate) fn add(&mut self, outpoint: OutPoint) -> Result<bool, WalletError> {
        let inserted = self.denied_outpoints.insert(outpoint);
        if inserted {
            self.save_to_disk()?;
        }
        Ok(inserted)
    }

    #[allow(dead_code)]
    pub(crate) fn add_batch(&mut self, outpoints: &[OutPoint]) -> Result<usize, WalletError> {
        let mut added = 0usize;
        for outpoint in outpoints {
            if self.denied_outpoints.insert(*outpoint) {
                added += 1;
            }
        }

        if added > 0 {
            self.save_to_disk()?;
        }

        Ok(added)
    }

    pub(crate) fn remove(&mut self, outpoint: &OutPoint) -> Result<bool, WalletError> {
        let removed = self.denied_outpoints.remove(outpoint);
        if removed {
            self.save_to_disk()?;
        }
        Ok(removed)
    }

    pub(crate) fn clear(&mut self) -> Result<usize, WalletError> {
        let removed = self.denied_outpoints.len();
        self.denied_outpoints.clear();
        self.save_to_disk()?;
        Ok(removed)
    }

    pub(crate) fn import_from_file(&mut self, import_path: &Path) -> Result<usize, WalletError> {
        let outpoints = Self::read_outpoints_from_file(import_path)?;
        let mut imported = 0usize;
        for outpoint in outpoints {
            if self.denied_outpoints.insert(outpoint) {
                imported += 1;
            }
        }

        if imported > 0 {
            self.save_to_disk()?;
        }

        Ok(imported)
    }

    pub(crate) fn first_blocked_input(&self, tx: &Transaction) -> Option<OutPoint> {
        tx.input
            .iter()
            .map(|txin| txin.previous_output)
            .find(|outpoint| self.denied_outpoints.contains(outpoint))
    }

    fn save_to_disk(&self) -> Result<(), WalletError> {
        let mut file = File::create(&self.path)?;
        for outpoint in self.list() {
            writeln!(file, "{outpoint}")?;
        }
        file.flush()?;
        Ok(())
    }

    fn read_outpoints_from_file(path: &Path) -> Result<HashSet<OutPoint>, WalletError> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let mut outpoints = HashSet::new();

        for (line_number, line) in reader.lines().enumerate() {
            let line = line?;
            let entry = line.trim();
            if entry.is_empty() || entry.starts_with('#') {
                continue;
            }

            let outpoint = OutPoint::from_str(entry).map_err(|_| {
                WalletError::General(format!(
                    "invalid outpoint '{}' at line {}",
                    entry,
                    line_number + 1
                ))
            })?;
            outpoints.insert(outpoint);
        }

        Ok(outpoints)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::{
        absolute::LockTime, transaction::Version, Amount, Sequence, TxIn, TxOut, Txid, Witness,
    };
    use std::{
        fs,
        path::PathBuf,
        str::FromStr,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn unique_test_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("valid time")
            .as_nanos();
        std::env::temp_dir().join(format!("coinswap-{name}-{nanos}.txt"))
    }

    fn test_outpoint(vout: u32) -> OutPoint {
        OutPoint::new(
            Txid::from_str("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
                .expect("valid txid"),
            vout,
        )
    }

    fn test_transaction_with_inputs(inputs: &[OutPoint]) -> Transaction {
        Transaction {
            version: Version::TWO,
            lock_time: LockTime::ZERO,
            input: inputs
                .iter()
                .map(|outpoint| TxIn {
                    previous_output: *outpoint,
                    script_sig: bitcoin::ScriptBuf::new(),
                    sequence: Sequence::MAX,
                    witness: Witness::default(),
                })
                .collect(),
            output: vec![TxOut {
                value: Amount::from_sat(1),
                script_pubkey: bitcoin::ScriptBuf::new(),
            }],
        }
    }

    #[test]
    fn add_remove_contains_and_idempotency() {
        let path = unique_test_path("add-remove");
        let mut deny_list = UtxoDenyList::load(path.clone()).expect("deny-list loaded");
        let outpoint = test_outpoint(0);

        assert!(!deny_list.contains(&outpoint));
        assert!(deny_list.add(outpoint).expect("add first"));
        assert!(!deny_list.add(outpoint).expect("add duplicate"));
        assert!(deny_list.contains(&outpoint));
        assert!(deny_list.remove(&outpoint).expect("remove existing"));
        assert!(!deny_list.remove(&outpoint).expect("remove missing"));
        assert!(!deny_list.contains(&outpoint));

        fs::remove_file(path).expect("cleanup");
    }

    #[test]
    fn import_with_comments_and_duplicates() {
        let deny_path = unique_test_path("import-deny");
        let import_path = unique_test_path("import-source");
        fs::write(
            &import_path,
            "\
# comment
0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef:0

0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef:1
0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef:1
",
        )
        .expect("write import");

        let mut deny_list = UtxoDenyList::load(deny_path.clone()).expect("deny-list loaded");
        let imported = deny_list
            .import_from_file(&import_path)
            .expect("import succeeds");
        assert_eq!(imported, 2);
        assert_eq!(deny_list.len(), 2);

        fs::remove_file(deny_path).expect("cleanup deny");
        fs::remove_file(import_path).expect("cleanup import");
    }

    #[test]
    fn clear_len_is_empty_semantics() {
        let path = unique_test_path("clear");
        let mut deny_list = UtxoDenyList::load(path.clone()).expect("deny-list loaded");
        deny_list.add(test_outpoint(0)).expect("add");
        deny_list.add(test_outpoint(1)).expect("add");
        assert_eq!(deny_list.len(), 2);
        assert!(!deny_list.is_empty());

        let cleared = deny_list.clear().expect("clear");
        assert_eq!(cleared, 2);
        assert_eq!(deny_list.len(), 0);
        assert!(deny_list.is_empty());

        fs::remove_file(path).expect("cleanup");
    }

    #[test]
    fn add_batch_is_deduplicated() {
        let path = unique_test_path("add-batch");
        let mut deny_list = UtxoDenyList::load(path.clone()).expect("deny-list loaded");
        let outpoints = vec![test_outpoint(0), test_outpoint(1), test_outpoint(1)];
        let added = deny_list.add_batch(&outpoints).expect("batch add");
        assert_eq!(added, 2);
        assert_eq!(deny_list.len(), 2);
        assert_eq!(deny_list.add_batch(&outpoints).expect("batch re-add"), 0);

        fs::remove_file(path).expect("cleanup");
    }

    #[test]
    fn persistence_across_reload() {
        let path = unique_test_path("persistence");
        {
            let mut deny_list = UtxoDenyList::load(path.clone()).expect("deny-list loaded");
            deny_list.add(test_outpoint(42)).expect("add");
        }

        let deny_list = UtxoDenyList::load(path.clone()).expect("deny-list reloaded");
        assert!(deny_list.contains(&test_outpoint(42)));

        fs::remove_file(path).expect("cleanup");
    }

    #[test]
    fn first_blocked_input_returns_first_match() {
        let path = unique_test_path("first-blocked");
        let mut deny_list = UtxoDenyList::load(path.clone()).expect("deny-list loaded");
        let blocked_1 = test_outpoint(5);
        let blocked_2 = test_outpoint(7);
        deny_list.add(blocked_1).expect("add blocked");
        deny_list.add(blocked_2).expect("add blocked");

        let tx = test_transaction_with_inputs(&[test_outpoint(3), blocked_2, blocked_1]);
        assert_eq!(deny_list.first_blocked_input(&tx), Some(blocked_2));

        fs::remove_file(path).expect("cleanup");
    }

    #[test]
    fn parse_error_contains_line_number() {
        let path = unique_test_path("parse-error");
        fs::write(
            &path,
            "\
# header
0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef:0
not-an-outpoint
",
        )
        .expect("write");

        let result = UtxoDenyList::load(path.clone());
        let error_string = match result {
            Ok(_) => panic!("expected parse error"),
            Err(err) => format!("{err:?}"),
        };

        assert!(error_string.contains("line 3"));

        fs::remove_file(path).expect("cleanup");
    }
}

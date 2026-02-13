use bitcoin::{OutPoint, Transaction};
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, BufRead, BufReader, Write},
    path::{Path, PathBuf},
    str::FromStr,
};

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
    pub(crate) fn load(path: PathBuf) -> io::Result<Self> {
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

    pub(crate) fn contains(&self, outpoint: &OutPoint) -> bool {
        self.denied_outpoints.contains(outpoint)
    }

    pub(crate) fn add(&mut self, outpoint: OutPoint) -> io::Result<bool> {
        let inserted = self.denied_outpoints.insert(outpoint);
        if inserted {
            self.save_to_disk()?;
        }
        Ok(inserted)
    }

    pub(crate) fn remove(&mut self, outpoint: &OutPoint) -> io::Result<bool> {
        let removed = self.denied_outpoints.remove(outpoint);
        if removed {
            self.save_to_disk()?;
        }
        Ok(removed)
    }

    pub(crate) fn import_from_file(&mut self, import_path: &Path) -> io::Result<usize> {
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

    fn save_to_disk(&self) -> io::Result<()> {
        let mut file = File::create(&self.path)?;
        for outpoint in self.list() {
            writeln!(file, "{outpoint}")?;
        }
        file.flush()?;
        Ok(())
    }

    fn read_outpoints_from_file(path: &Path) -> io::Result<HashSet<OutPoint>> {
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
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid outpoint '{}' at line {}", entry, line_number + 1),
                )
            })?;
            outpoints.insert(outpoint);
        }

        Ok(outpoints)
    }
}

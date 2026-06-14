use crate::wallet::api::KeychainKind;
use crate::wallet::rpc::RPCConfig;
use crate::wallet::storage::AddressType;
use bitcoin::{Network, ScriptBuf};
use rand::seq::IteratorRandom;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// BlockchainBackend Trait — Abstracting the Sync Layer
pub trait BlockchainBackend: Send + Sync {
    // Abstract methods for blockchain interaction would go here.
    // e.g. fn get_tx_out(&self, txid: &Txid, vout: u32) -> Result<Option<TxOut>, WalletError>;
}

/// Backend configuration — determines how the wallet talks to the blockchain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BackendConfig {
    /// Full Bitcoin Core RPC (for makers, or takers with a local node).
    Rpc(RPCConfig),
    /// Electrum server pool with privacy parameters (for mobile takers).
    Electrum(ElectrumConfig),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElectrumConfig {
    pub servers: Vec<String>,
    pub tor_proxy: Option<String>,
    /// Range for decoy addresses per batch — random count drawn each batch.
    pub decoy_pool_range: (usize, usize),   // 34 - 67
    /// Range for real addresses per batch — random count drawn each batch.
    pub real_batch_range: (usize, usize),  // 3 - 9
    /// Max decoys to keep in persistent cache.
    pub max_decoy_cache_size: usize,       // 500
    /// Fraction of decoys to replace with fresh ones per sync.
    pub decoy_rotation_range: (f64, f64), // (min, max) : (0.27, 0.53)
    pub network: Network,
}

pub struct ElectrumBackend {
    /// Pool of Electrum server URLs
    pub servers: Vec<String>,
    /// Range for decoy addresses per batch
    pub decoy_pool_range: (usize, usize),
    /// Range for real addresses per batch
    pub real_batch_range: (usize, usize),
    /// Rotate server connection per batch
    pub rotate_servers: bool,
    /// Persistent decoy cache
    pub decoy_cache: DecoyCache,
}

/// Cached decoy scripts with usage metadata for rotation decisions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecoyCache {
    /// All currently cached decoys with per-entry metadata.
    pub entries: Vec<DecoyEntry>,
    /// Max total decoys to keep in the cache at any time.
    pub max_cache_size: usize,
    /// Fraction of cache to retire (replace with fresh) on each sync.
    pub rotation_range: (f64, f64),
    /// Maximum number of syncs a single decoy can survive before forced retirement.
    pub max_uses: u32,
    /// Maximum age (in seconds) before a decoy is force-retired.
    pub max_age_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecoyEntry {
    pub script: ScriptBuf,
    pub hd_index: u32,
    pub keychain: KeychainKind,
    pub addr_type: AddressType,
    /// How many sync batches this decoy has appeared in.
    pub times_used: u32,
    /// Timestamp when the decoy was first created.
    pub created_at: u64,
    /// Per-entry max-use limit (randomized at creation).
    pub retire_after_uses: u32,
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

impl DecoyCache {
    /// Select decoys for a single batch: mix of cached + fresh.
    /// Returns the scripts to use and mutates the cache state.
    pub fn select_decoys_for_batch(&mut self, needed: usize) -> Vec<ScriptBuf> {
        let mut rng = rand::thread_rng();
        let now = now_unix_secs();

        // 1. Retire expired entries
        self.entries.retain(|e| {
            e.times_used < e.retire_after_uses && (now - e.created_at) < self.max_age_secs
        });

        // 2. Determine fresh vs reused split
        let rotation_pct = rng.gen_range(self.rotation_range.0..=self.rotation_range.1);
        let fresh_count = (needed as f64 * rotation_pct).floor() as usize;
        let reused_count = needed.saturating_sub(fresh_count);

        // 3. Sample reused decoys from cache (random subset)
        let reused_count = reused_count.min(self.entries.len());
        let reused_indices: Vec<usize> = (0..self.entries.len())
            .choose_multiple(&mut rng, reused_count);
        let mut result = Vec::with_capacity(needed);

        for &idx in &reused_indices {
            self.entries[idx].times_used += 1;
            result.push(self.entries[idx].script.clone());
        }

        // 4. Generate fresh decoys (pure HD derivation, no network)
        let actual_fresh = needed - result.len();
        for _ in 0..actual_fresh {
            let entry = self.derive_fresh_decoy();
            result.push(entry.script.clone());
            self.entries.push(entry);
        }

        // 5. Trim cache if over max size
        while self.entries.len() > self.max_cache_size {
            // Drop the most-used + oldest entry
            if let Some(idx) = self.entries.iter().enumerate()
                .max_by_key(|(_, e)| e.times_used as u64 * 1000 + (now - e.created_at))
                .map(|(i, _)| i)
            {
                self.entries.swap_remove(idx);
            }
        }

        result
    }

    /// Mock generation of a fresh decoy
    fn derive_fresh_decoy(&mut self) -> DecoyEntry {
        let mut rng = rand::thread_rng();
        DecoyEntry {
            script: ScriptBuf::new(), // Requires access to wallet master key to actually derive
            hd_index: rng.gen_range(10000..20000), // Derive a far-future unused index
            keychain: KeychainKind::External, 
            addr_type: AddressType::P2WPKH,
            times_used: 0,
            created_at: now_unix_secs(),
            retire_after_uses: rng.gen_range(5..=15),
        }
    }
}

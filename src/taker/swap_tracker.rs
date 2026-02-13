//! Persistent swap tracker for crash-resilient recovery.
//!
//! Stores swap state to `{data_dir}/swap_tracker.cbor` using atomic writes
//! (write-to-tmp then rename). This ensures the taker can resume recovery
//! after a crash at any point during a swap.
#![allow(missing_docs)]

use std::{
    collections::HashMap,
    convert::TryInto,
    fmt,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use bitcoin::{secp256k1::SecretKey, Txid};
use serde::{Deserialize, Serialize};

use crate::protocol::common_messages::ProtocolVersion;

use super::error::TakerError;

/// Newtype wrapper for `SecretKey` that implements Serialize/Deserialize.
///
/// `SecretKey` doesn't implement serde traits, so we serialize as raw 32-byte array.
#[derive(Debug, Clone)]
pub struct SerializableSecretKey(pub SecretKey);

impl Serialize for SerializableSecretKey {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Serialize as a fixed-size array of 32 bytes for CBOR compatibility
        let bytes: [u8; 32] = self.0[..].try_into().expect("SecretKey is always 32 bytes");
        bytes.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SerializableSecretKey {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let bytes: [u8; 32] = Deserialize::deserialize(deserializer)?;
        let key = SecretKey::from_slice(&bytes).map_err(serde::de::Error::custom)?;
        Ok(SerializableSecretKey(key))
    }
}

impl From<SecretKey> for SerializableSecretKey {
    fn from(key: SecretKey) -> Self {
        SerializableSecretKey(key)
    }
}

impl From<SerializableSecretKey> for SecretKey {
    fn from(key: SerializableSecretKey) -> Self {
        key.0
    }
}

/// Tracks which phase of the swap lifecycle has been reached.
///
/// Derives `PartialOrd`/`Ord` so phase comparisons like `phase >= FundsBroadcast`
/// work naturally. Variant order matters — they must be listed in lifecycle order.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default, Hash,
)]
pub enum SwapPhase {
    /// Makers selected, negotiation underway. Safe to abort.
    #[default]
    Negotiating,
    /// Outgoing swapcoins created, not yet broadcast. Safe to abort.
    FundingCreated,
    /// On-chain. Point of no return — recovery needed on failure.
    FundsBroadcast,
    /// All makers responded with contracts.
    ContractsExchanged,
    /// Preimage reveal in progress.
    Finalizing,
    /// All maker privkeys received by taker.
    PrivkeysCollected,
    /// Inter-maker privkey forwarding done.
    PrivkeysForwarded,
    /// Swap finished, incoming swept.
    Completed,
    /// Swap failed (see `failed_at_phase` for context).
    Failed,
}

impl fmt::Display for SwapPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SwapPhase::Negotiating => write!(f, "Negotiating"),
            SwapPhase::FundingCreated => write!(f, "FundingCreated"),
            SwapPhase::FundsBroadcast => write!(f, "FundsBroadcast"),
            SwapPhase::ContractsExchanged => write!(f, "ContractsExchanged"),
            SwapPhase::Finalizing => write!(f, "Finalizing"),
            SwapPhase::PrivkeysCollected => write!(f, "PrivkeysCollected"),
            SwapPhase::PrivkeysForwarded => write!(f, "PrivkeysForwarded"),
            SwapPhase::Completed => write!(f, "Completed"),
            SwapPhase::Failed => write!(f, "Failed"),
        }
    }
}

/// Per-maker milestone tracking within a swap.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MakerProgress {
    pub address: String,
    pub negotiated: bool,
    pub funding_confirmed: bool,
    pub contracts_exchanged: bool,
    pub preimage_revealed: bool,
    pub privkey_received: bool,
    pub privkey_forwarded: bool,
}

/// Tracks recovery progress across crashes.
///
/// Derives `PartialOrd`/`Ord` so recovery can skip completed phases with
/// `if recovery.phase < RecoveryPhase::PreimageStamped { ... }`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default, Hash,
)]
pub enum RecoveryPhase {
    #[default]
    NotStarted,
    /// Phase 1 done: preimage set on incoming swapcoins in memory.
    PreimageStamped,
    /// Phase 2 done: swapcoins written to wallet on disk.
    SwapcoinsPersisted,
    /// Phase 3 done: hashlock sweep attempted for all incoming.
    IncomingRecovered,
    /// Phase 4 done: timelock recovery attempted for all outgoing.
    OutgoingRecovered,
    /// Phase 5 done: watch-only removed, swap state cleared.
    CleanedUp,
}

/// Tracks per-swapcoin recovery results.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RecoveryState {
    pub phase: RecoveryPhase,
    /// Txids of incoming contracts successfully swept via hashlock.
    pub incoming_swept: Vec<Txid>,
    /// Txids of incoming contracts registered for watch (sweep failed or deferred).
    pub incoming_watching: Vec<Txid>,
    /// Txids of outgoing contracts successfully recovered via timelock.
    pub outgoing_recovered: Vec<Txid>,
    /// Txids of outgoing contracts registered for watch (timelock not mature).
    pub outgoing_watching: Vec<Txid>,
}

/// A persistent record of a single swap's state and progress.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwapRecord {
    pub swap_id: String,
    pub preimage: [u8; 32],
    pub protocol: ProtocolVersion,
    pub send_amount_sat: u64,
    pub maker_count: usize,
    pub phase: SwapPhase,
    pub failed_at_phase: Option<SwapPhase>,
    pub failure_reason: Option<String>,
    pub makers: Vec<MakerProgress>,
    pub outgoing_contract_txids: Vec<Txid>,
    pub incoming_contract_txids: Vec<Txid>,
    pub watchonly_contract_txids: Vec<Txid>,
    pub recovery: RecoveryState,
    /// Multisig nonces needed for ProofOfFunding during recovery (Legacy only).
    /// Empty for Taproot swaps.
    pub multisig_nonces: Vec<SerializableSecretKey>,
    /// Hashlock nonces for recovery (both Legacy and Taproot).
    /// Legacy: used in ProofOfFunding. Taproot: one per maker hop.
    pub hashlock_nonces: Vec<SerializableSecretKey>,
    pub created_at: u64,
    pub updated_at: u64,
}

impl fmt::Display for RecoveryPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

/// Truncate each txid to its first 8 hex characters for compact display.
fn short_txids(txids: &[Txid]) -> Vec<String> {
    txids
        .iter()
        .map(|txid| {
            let s = txid.to_string();
            s[..8.min(s.len())].to_string()
        })
        .collect()
}

impl fmt::Display for MakerProgress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let flags = [
            ("N", self.negotiated),
            ("F", self.funding_confirmed),
            ("C", self.contracts_exchanged),
            ("P", self.preimage_revealed),
            ("K", self.privkey_received),
            ("W", self.privkey_forwarded),
        ];
        let status: String = flags
            .iter()
            .map(|(c, ok)| if *ok { *c } else { "-" })
            .collect();
        write!(f, "{}[{}]", self.address, status)
    }
}

impl fmt::Display for RecoveryState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "phase={}", self.phase)?;
        if !self.incoming_swept.is_empty() {
            write!(f, " in_swept={:?}", short_txids(&self.incoming_swept))?;
        }
        if !self.incoming_watching.is_empty() {
            write!(f, " in_watch={:?}", short_txids(&self.incoming_watching))?;
        }
        if !self.outgoing_recovered.is_empty() {
            write!(
                f,
                " out_recovered={:?}",
                short_txids(&self.outgoing_recovered)
            )?;
        }
        if !self.outgoing_watching.is_empty() {
            write!(f, " out_watch={:?}", short_txids(&self.outgoing_watching))?;
        }
        Ok(())
    }
}

impl fmt::Display for SwapRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{}] phase={} proto={:?} amt={} makers={}",
            self.swap_id, self.phase, self.protocol, self.send_amount_sat, self.maker_count,
        )?;
        if let Some(ref failed_at) = self.failed_at_phase {
            write!(f, " failed_at={}", failed_at)?;
        }
        write!(
            f,
            " out_txids={} in_txids={} wo_txids={}",
            self.outgoing_contract_txids.len(),
            self.incoming_contract_txids.len(),
            self.watchonly_contract_txids.len(),
        )?;
        if !self.makers.is_empty() {
            write!(f, "\n  makers:")?;
            for (i, m) in self.makers.iter().enumerate() {
                write!(f, "\n    [{}] {}", i, m)?;
            }
        }
        write!(f, "\n  recovery: {}", self.recovery)?;
        Ok(())
    }
}

/// Internal storage format for the tracker.
#[derive(Serialize, Deserialize, Default)]
struct SwapTrackerData {
    swaps: HashMap<String, SwapRecord>,
}

/// Persistent swap tracker backed by a CBOR file with atomic writes.
pub struct SwapTracker {
    path: PathBuf,
    data: SwapTrackerData,
}

impl SwapTracker {
    /// Load tracker from disk or create a new empty one.
    pub fn load_or_create(data_dir: &Path) -> Result<Self, TakerError> {
        let path = data_dir.join("swap_tracker.cbor");
        let data = if path.exists() {
            match std::fs::read(&path) {
                Ok(bytes) => serde_cbor::from_slice(&bytes).unwrap_or_default(),
                Err(e) => {
                    log::warn!("Failed to read swap tracker at {:?}: {}", path, e);
                    SwapTrackerData::default()
                }
            }
        } else {
            SwapTrackerData::default()
        };

        Ok(Self { path, data })
    }

    /// Atomic flush: write to tmp file, then rename over original.
    fn flush(&self) -> Result<(), TakerError> {
        let tmp_path = self.path.with_extension("cbor.tmp");

        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let bytes = serde_cbor::to_vec(&self.data)
            .map_err(|e| TakerError::General(format!("Failed to serialize swap tracker: {}", e)))?;

        std::fs::write(&tmp_path, &bytes)?;
        std::fs::rename(&tmp_path, &self.path)?;

        Ok(())
    }

    /// Upsert a swap record and flush to disk.
    pub fn save_record(&mut self, record: &SwapRecord) -> Result<(), TakerError> {
        self.data
            .swaps
            .insert(record.swap_id.clone(), record.clone());
        self.flush()
    }

    /// Remove a swap record and flush to disk.
    pub fn remove_record(&mut self, swap_id: &str) -> Result<(), TakerError> {
        self.data.swaps.remove(swap_id);
        self.flush()
    }

    /// Returns all non-Completed swap records.
    ///
    /// Includes `Failed` records where `recovery.phase < CleanedUp`
    /// (crash during recovery) so recovery can resume.
    pub fn incomplete_swaps(&self) -> Vec<&SwapRecord> {
        self.data
            .swaps
            .values()
            .filter(|r| {
                r.phase != SwapPhase::Completed
                    && !(r.phase == SwapPhase::Failed
                        && r.recovery.phase >= RecoveryPhase::CleanedUp)
            })
            .collect()
    }

    /// Prune old Completed and fully-cleaned-up Failed records.
    #[allow(dead_code)]
    pub fn cleanup_old_records(&mut self, max_age_secs: u64) -> Result<(), TakerError> {
        let now = now_secs();
        self.data.swaps.retain(|_, r| {
            let dominated = r.phase == SwapPhase::Completed
                || (r.phase == SwapPhase::Failed && r.recovery.phase >= RecoveryPhase::CleanedUp);
            if dominated {
                now.saturating_sub(r.updated_at) < max_age_secs
            } else {
                true
            }
        });
        self.flush()
    }

    /// Get a reference to a swap record by ID.
    pub fn get_record(&self, swap_id: &str) -> Option<&SwapRecord> {
        self.data.swaps.get(swap_id)
    }

    /// Get a mutable reference to a swap record by ID.
    pub fn get_record_mut(&mut self, swap_id: &str) -> Option<&mut SwapRecord> {
        self.data.swaps.get_mut(swap_id)
    }

    /// Log all swap records at INFO level.
    pub fn log_state(&self) {
        if self.data.swaps.is_empty() {
            log::info!("[SwapTracker] (empty — no records)");
            return;
        }
        for record in self.data.swaps.values() {
            for line in format!("{}", record).lines() {
                log::info!("[SwapTracker] {}", line);
            }
        }
    }
}

impl fmt::Display for SwapTracker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.data.swaps.is_empty() {
            return write!(f, "[SwapTracker] (empty)");
        }
        for (i, record) in self.data.swaps.values().enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            write!(f, "[SwapTracker] {}", record)?;
        }
        Ok(())
    }
}

/// Current time as seconds since UNIX epoch.
pub(crate) fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoind::tempfile::TempDir;

    fn make_test_record(swap_id: &str, phase: SwapPhase) -> SwapRecord {
        SwapRecord {
            swap_id: swap_id.to_string(),
            preimage: [0u8; 32],
            protocol: ProtocolVersion::Legacy,
            send_amount_sat: 100_000,
            maker_count: 2,
            phase,
            failed_at_phase: None,
            failure_reason: None,
            makers: vec![],
            outgoing_contract_txids: vec![],
            incoming_contract_txids: vec![],
            watchonly_contract_txids: vec![],
            recovery: RecoveryState::default(),
            multisig_nonces: vec![],
            hashlock_nonces: vec![],
            created_at: now_secs(),
            updated_at: now_secs(),
        }
    }

    #[test]
    fn test_load_or_create_empty() {
        let dir = TempDir::new().unwrap();
        let tracker = SwapTracker::load_or_create(dir.path()).unwrap();
        assert!(tracker.incomplete_swaps().is_empty());
    }

    #[test]
    fn test_save_and_reload() {
        let dir = TempDir::new().unwrap();

        {
            let mut tracker = SwapTracker::load_or_create(dir.path()).unwrap();
            let record = make_test_record("swap1", SwapPhase::FundsBroadcast);
            tracker.save_record(&record).unwrap();
        }

        let tracker = SwapTracker::load_or_create(dir.path()).unwrap();
        let incomplete = tracker.incomplete_swaps();
        assert_eq!(incomplete.len(), 1);
        assert_eq!(incomplete[0].swap_id, "swap1");
        assert_eq!(incomplete[0].phase, SwapPhase::FundsBroadcast);
    }

    #[test]
    fn test_remove_record() {
        let dir = TempDir::new().unwrap();
        let mut tracker = SwapTracker::load_or_create(dir.path()).unwrap();

        let record = make_test_record("swap1", SwapPhase::Negotiating);
        tracker.save_record(&record).unwrap();
        assert_eq!(tracker.incomplete_swaps().len(), 1);

        tracker.remove_record("swap1").unwrap();
        assert!(tracker.incomplete_swaps().is_empty());
    }

    #[test]
    fn test_completed_not_in_incomplete() {
        let dir = TempDir::new().unwrap();
        let mut tracker = SwapTracker::load_or_create(dir.path()).unwrap();

        let record = make_test_record("swap1", SwapPhase::Completed);
        tracker.save_record(&record).unwrap();
        assert!(tracker.incomplete_swaps().is_empty());
    }

    #[test]
    fn test_failed_with_cleanup_not_in_incomplete() {
        let dir = TempDir::new().unwrap();
        let mut tracker = SwapTracker::load_or_create(dir.path()).unwrap();

        let mut record = make_test_record("swap1", SwapPhase::Failed);
        record.recovery.phase = RecoveryPhase::CleanedUp;
        tracker.save_record(&record).unwrap();
        assert!(tracker.incomplete_swaps().is_empty());
    }

    #[test]
    fn test_failed_without_cleanup_in_incomplete() {
        let dir = TempDir::new().unwrap();
        let mut tracker = SwapTracker::load_or_create(dir.path()).unwrap();

        let mut record = make_test_record("swap1", SwapPhase::Failed);
        record.recovery.phase = RecoveryPhase::PreimageStamped;
        tracker.save_record(&record).unwrap();
        assert_eq!(tracker.incomplete_swaps().len(), 1);
    }

    #[test]
    fn test_phase_ordering() {
        assert!(SwapPhase::Negotiating < SwapPhase::FundingCreated);
        assert!(SwapPhase::FundingCreated < SwapPhase::FundsBroadcast);
        assert!(SwapPhase::FundsBroadcast < SwapPhase::ContractsExchanged);
        assert!(SwapPhase::ContractsExchanged < SwapPhase::Finalizing);
        assert!(SwapPhase::Finalizing < SwapPhase::PrivkeysCollected);
        assert!(SwapPhase::PrivkeysCollected < SwapPhase::PrivkeysForwarded);
        assert!(SwapPhase::PrivkeysForwarded < SwapPhase::Completed);
    }

    #[test]
    fn test_recovery_phase_ordering() {
        assert!(RecoveryPhase::NotStarted < RecoveryPhase::PreimageStamped);
        assert!(RecoveryPhase::PreimageStamped < RecoveryPhase::SwapcoinsPersisted);
        assert!(RecoveryPhase::SwapcoinsPersisted < RecoveryPhase::IncomingRecovered);
        assert!(RecoveryPhase::IncomingRecovered < RecoveryPhase::OutgoingRecovered);
        assert!(RecoveryPhase::OutgoingRecovered < RecoveryPhase::CleanedUp);
    }

    #[test]
    fn test_cleanup_old_records() {
        let dir = TempDir::new().unwrap();
        let mut tracker = SwapTracker::load_or_create(dir.path()).unwrap();

        let mut old_complete = make_test_record("old", SwapPhase::Completed);
        old_complete.updated_at = 0; // epoch = very old
        tracker.save_record(&old_complete).unwrap();

        let active = make_test_record("active", SwapPhase::FundsBroadcast);
        tracker.save_record(&active).unwrap();

        tracker.cleanup_old_records(3600).unwrap(); // 1 hour max age

        assert!(tracker.get_record("old").is_none());
        assert!(tracker.get_record("active").is_some());
    }

    #[test]
    fn test_serializable_secret_key() {
        use bitcoin::secp256k1::rand::rngs::OsRng;

        let key = SecretKey::new(&mut OsRng);
        let serializable = SerializableSecretKey::from(key);

        let bytes = serde_cbor::to_vec(&serializable).unwrap();
        let deserialized: SerializableSecretKey = serde_cbor::from_slice(&bytes).unwrap();

        assert_eq!(key, deserialized.0);
    }

    #[test]
    fn test_atomic_write_creates_no_tmp_on_success() {
        let dir = TempDir::new().unwrap();
        let mut tracker = SwapTracker::load_or_create(dir.path()).unwrap();

        let record = make_test_record("swap1", SwapPhase::Negotiating);
        tracker.save_record(&record).unwrap();

        let tmp_path = dir.path().join("swap_tracker.cbor.tmp");
        assert!(
            !tmp_path.exists(),
            "tmp file should be removed after rename"
        );
    }
}

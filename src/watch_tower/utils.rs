//! Utility helpers for parsing watchtower-relevant transactions and updating registry state.

use std::{
    collections::{HashSet, VecDeque},
    str::FromStr,
};

use bitcoin::{
    absolute::{Height, LockTime},
    Block, Transaction, Txid,
};

use crate::{
    wallet::{MAX_FIDELITY_TIMELOCK, MIN_FIDELITY_TIMELOCK},
    watch_tower::{registry_storage::FileRegistry, watcher::Role, watcher_error::WatcherError},
};

/// Maximum number of txids to track for cache.
const MAX_SEEN_TXIDS: usize = 5_000;
/// Maximum payload size for a 56-byte onion label, `#`, and a 10-digit expiry height.
const MAX_FIDELITY_ANNOUNCEMENT_BYTES: usize = 67;
/// Minimum bond value accepted for maker discovery to prevent cheap dust-bond spam.
const MIN_FIDELITY_BOND_AMOUNT_SATS: u64 = 10_000;

/// Bounded deduplication.
/// Combines HashSet for O(1) lookup with VecDeque for ordering.
pub(crate) struct SeenTxids {
    /// Fast lookup: whether we've seen a txid
    seen: HashSet<Txid>,
    /// Tracks insertion order FIFO
    order: VecDeque<Txid>,
    /// Txids claimed by a thread for validation. Other threads will skip this.
    in_flight: HashSet<Txid>,
}

/// Fidelity announcement done by watcher to registry
#[derive(Debug)]
pub struct FidelityAnnouncement {
    /// Onion address
    pub onion: String,
    /// Fidelity expire height
    pub expires_at_height: u32,
}

fn extract_op_return_data(script: &[u8]) -> Option<&[u8]> {
    if script.first()? != &0x6a {
        return None; // OP_RETURN
    }

    let data_len = *script.get(1)? as usize;
    if !(1..=MAX_FIDELITY_ANNOUNCEMENT_BYTES).contains(&data_len) || script.len() != data_len + 2 {
        return None;
    }
    script.get(2..)
}

/// Validates the canonical maker-address representation used outside the
/// fidelity announcement payload.
pub(crate) fn is_valid_maker_address(address: &str) -> bool {
    #[cfg(not(feature = "integration-test"))]
    {
        let Some(label) = address.strip_suffix(".onion") else {
            return false;
        };
        label.len() == 56
            && label
                .bytes()
                .all(|byte| matches!(byte, b'a'..=b'z' | b'2'..=b'7'))
    }

    #[cfg(feature = "integration-test")]
    {
        let Some((ip, port)) = address.split_once(':') else {
            return false;
        };
        ip.parse::<std::net::Ipv4Addr>().is_ok()
            && matches!(port.parse::<u16>(), Ok(port) if port > 0)
    }
}

fn normalize_onion_address(s: &str) -> Option<String> {
    #[cfg(not(feature = "integration-test"))]
    {
        let onion = s.strip_suffix(".onion").unwrap_or(s);
        let address = format!("{onion}.onion");
        is_valid_maker_address(&address).then_some(address)
    }

    #[cfg(feature = "integration-test")]
    {
        is_valid_maker_address(s).then(|| s.to_string())
    }
}

fn parse_fidelity_op_return(data: &[u8]) -> Option<FidelityAnnouncement> {
    if data.len() > MAX_FIDELITY_ANNOUNCEMENT_BYTES {
        return None;
    }
    let decoded = std::str::from_utf8(data).ok()?;
    let (endpoint, locktime_str) = decoded.split_once('#')?;
    if locktime_str.is_empty()
        || !locktime_str.bytes().all(|byte| byte.is_ascii_digit())
        || (locktime_str.len() > 1 && locktime_str.starts_with('0'))
    {
        return None;
    }
    let expires_at_height = locktime_str.parse::<u32>().ok()?;
    let onion = normalize_onion_address(endpoint)?;

    Some(FidelityAnnouncement {
        onion,
        expires_at_height,
    })
}

/// Process a transaction for fidelity OP_RETURN announcement.
pub fn process_fidelity(tx: &Transaction) -> Option<FidelityAnnouncement> {
    let tx_height = match tx.lock_time {
        LockTime::Blocks(height) if height != Height::ZERO => height.to_consensus_u32(),
        _ => return None,
    };
    if tx.input.is_empty()
        || tx
            .input
            .iter()
            .all(|input| input.sequence == bitcoin::Sequence::MAX)
        || !(2..=3).contains(&tx.output.len())
    {
        return None;
    }
    let bond = tx.output.first()?;
    if !bond.script_pubkey.is_p2wsh() || bond.value.to_sat() < MIN_FIDELITY_BOND_AMOUNT_SATS {
        return None;
    }
    let announcement_output = tx.output.get(1)?;
    if announcement_output.value.to_sat() != 0 {
        return None;
    }
    let data = extract_op_return_data(announcement_output.script_pubkey.as_bytes())?;
    let announcement = parse_fidelity_op_return(data)?;
    let relative_timelock = announcement.expires_at_height.checked_sub(tx_height)?;
    if !(MIN_FIDELITY_TIMELOCK..=MAX_FIDELITY_TIMELOCK).contains(&relative_timelock) {
        return None;
    }
    Some(announcement)
}

/// Processes each transaction in a block, updating watch entries and recording fidelity data.
pub fn process_block<R: Role>(
    block: Block,
    registry: &mut FileRegistry,
) -> Result<(), WatcherError> {
    for tx in block.txdata.iter() {
        process_transaction(tx, registry, true)?;
        if R::RUN_DISCOVERY {
            let fidelity_announcement = process_fidelity(tx);
            if let Some(fidelity_announcement) = fidelity_announcement {
                let txid = tx.compute_txid();
                if registry.insert_fidelity(txid, fidelity_announcement)? {
                    log::info!("Stored verified fidelity via blockchain: {}", txid);
                }
            }
        }
    }
    Ok(())
}

/// Updates the registry for a transaction by clearing spent fidelities and marking watched spends.
pub fn process_transaction(
    tx: &Transaction,
    registry: &mut FileRegistry,
    in_block: bool,
) -> Result<(), WatcherError> {
    let watch_requests = registry.list_watches()?;
    for input in &tx.input {
        let outpoint = input.previous_output;
        for watch_request in &watch_requests {
            if outpoint != watch_request.outpoint {
                continue;
            }
            if let Some(recorded) = &watch_request.spent_tx {
                let recorded_txid = recorded.compute_txid();
                if recorded_txid != tx.compute_txid() {
                    log::warn!(
                        "conflicting spend of {outpoint}: recorded {recorded_txid} (confirmed={}), now seen {} (confirmed={in_block})",
                        watch_request.in_block,
                        tx.compute_txid()
                    );
                    // A confirmed spend can hold the only copy of a preimage, so
                    // a mempool-only rival must not overwrite it.
                    if watch_request.in_block && !in_block {
                        continue;
                    }
                }
            }
            let mut watch_request = watch_request.clone();
            watch_request.spent_tx = Some(tx.clone());
            watch_request.in_block = in_block;
            registry.upsert_watch(&watch_request)?;
            #[cfg(debug_assertions)]
            log::debug!(
                "[WATCH_STATE] Source: watch_tower::utils::process_transaction | Action: watched_outpoint_spent | Outpoint: {} | SpendingTxid: {} | Confirmed: {}",
                outpoint,
                tx.compute_txid(),
                in_block
            );
        }
    }
    Ok(())
}

pub(crate) fn parse_fidelity_event(event: &nostr::Event) -> Option<(Txid, u32)> {
    let content = event.content.trim();
    let (txid, vout) = content.split_once(':')?;

    let txid = Txid::from_str(txid).ok()?;
    let vout = vout.parse::<u32>().ok()?;

    Some((txid, vout))
}

impl SeenTxids {
    pub fn new() -> Self {
        Self {
            seen: HashSet::new(),
            order: VecDeque::new(),
            in_flight: HashSet::new(),
        }
    }

    /// Claims `txid` for processing. Returns false if it is already seen or
    /// claimed by another thread, so only one thread does the work.
    pub fn claim(&mut self, txid: Txid) -> bool {
        !self.seen.contains(&txid) && self.in_flight.insert(txid)
    }

    /// Drops a claim without marking it seen, leaving the txid open for retry.
    pub fn release(&mut self, txid: &Txid) {
        self.in_flight.remove(txid);
    }

    /// Returns true if txid was newly inserted (not seen before).
    /// Returns false if txid was already present.
    /// Uses FIFO eviction when capacity is exceeded.
    pub fn insert(&mut self, txid: Txid) -> bool {
        self.in_flight.remove(&txid);
        if self.seen.insert(txid) {
            self.order.push_back(txid);

            // Enforce capacity bound by evicting oldest entry
            if self.order.len() > MAX_SEEN_TXIDS {
                // Batch remove 10
                for _ in 0..10 {
                    if let Some(old) = self.order.pop_front() {
                        self.seen.remove(&old);
                    }
                }
            }

            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watch_tower::registry_storage::{FileRegistry, WatchRequest};
    use bitcoin::{
        absolute::{Height, LockTime},
        hashes::Hash,
        transaction, Amount, OutPoint, ScriptBuf, Sequence, TxIn, TxOut, Txid, Witness,
    };

    fn op_return(data: &[u8]) -> Vec<u8> {
        let mut script = vec![0x6a, data.len() as u8];
        script.extend_from_slice(data);
        script
    }

    fn p2wsh_script() -> ScriptBuf {
        let mut script = vec![0x00, 0x20];
        script.extend_from_slice(&[1u8; 32]);
        script.into()
    }

    fn announcement(lock: u32) -> Vec<u8> {
        let expiry = lock + MIN_FIDELITY_TIMELOCK;
        #[cfg(not(feature = "integration-test"))]
        return format!("{}#{expiry}", "a".repeat(56)).into_bytes();
        #[cfg(feature = "integration-test")]
        return format!("127.0.0.1:9050#{expiry}").into_bytes();
    }

    fn tx(lock: u32, inputs: Vec<OutPoint>, outputs: Vec<ScriptBuf>) -> Transaction {
        Transaction {
            version: transaction::Version(2),
            lock_time: LockTime::Blocks(
                Height::from_consensus(lock).expect("Invalid height value"),
            ),
            input: inputs
                .into_iter()
                .map(|op| TxIn {
                    previous_output: op,
                    script_sig: ScriptBuf::new(),
                    sequence: Sequence::MAX,
                    witness: Witness::new(),
                })
                .collect(),
            output: outputs
                .into_iter()
                .map(|spk| TxOut {
                    value: Amount::ZERO,
                    script_pubkey: spk,
                })
                .collect(),
        }
    }

    fn fidelity_tx(lock: u32, payload: &[u8]) -> Transaction {
        let mut transaction = tx(
            lock,
            vec![OutPoint::null()],
            vec![p2wsh_script(), op_return(payload).into()],
        );
        transaction.input[0].sequence = Sequence::ZERO;
        transaction.output[0].value = Amount::from_sat(MIN_FIDELITY_BOND_AMOUNT_SATS);
        transaction
    }

    #[test]
    fn test_maker_address_validation() {
        #[cfg(not(feature = "integration-test"))]
        {
            let valid = format!("{}.onion", "a".repeat(56));
            assert!(is_valid_maker_address(&valid));
            for invalid in [
                "",
                "abc.onion",
                "<script>aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.onion",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1.onion",
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA.onion",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.onion:6102",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.onion/path",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa .onion",
                "éééééééééééééééééééééééééééééééééééééééééééééééééééééééé.onion",
            ] {
                assert!(!is_valid_maker_address(invalid), "accepted {:?}", invalid);
            }
        }

        #[cfg(feature = "integration-test")]
        {
            assert!(is_valid_maker_address("127.0.0.1:6102"));
            for invalid in [
                "127.0.0.1",
                "127.0.0.1:0",
                "127.0.0.1:65536",
                "localhost:6102",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.onion",
            ] {
                assert!(!is_valid_maker_address(invalid), "accepted {:?}", invalid);
            }
        }
    }

    #[test]
    fn test_process_fidelity_valid() {
        let lock = 500;
        let tx = fidelity_tx(lock, &announcement(lock));

        let ann = process_fidelity(&tx).expect("expected valid fidelity announcement");

        #[cfg(not(feature = "integration-test"))]
        assert_eq!(ann.onion, format!("{}.onion", "a".repeat(56)));
        #[cfg(feature = "integration-test")]
        assert_eq!(ann.onion, "127.0.0.1:9050");
        assert_eq!(ann.expires_at_height, lock + MIN_FIDELITY_TIMELOCK);
    }

    #[cfg(not(feature = "integration-test"))]
    #[test]
    fn test_process_fidelity_rejects_noncanonical_onion_suffix_in_payload() {
        let lock = 500;
        let payload = format!("{}.onion#{}", "a".repeat(56), lock + MIN_FIDELITY_TIMELOCK);
        assert!(process_fidelity(&fidelity_tx(lock, payload.as_bytes())).is_none());
    }

    #[test]
    fn test_process_fidelity_invalid() {
        let lock = 500;
        let valid_payload = announcement(lock);

        let tx0 = fidelity_tx(0, &valid_payload);
        assert!(process_fidelity(&tx0).is_none());

        let tx1 = tx(
            lock,
            vec![OutPoint::null()],
            vec![op_return(&valid_payload).into()],
        );
        assert!(process_fidelity(&tx1).is_none());

        let tx4 = tx(
            lock,
            vec![OutPoint::null()],
            vec![
                p2wsh_script(),
                op_return(&valid_payload).into(),
                ScriptBuf::new(),
                ScriptBuf::new(),
            ],
        );
        assert!(process_fidelity(&tx4).is_none());

        let mut tx_no = fidelity_tx(lock, &valid_payload);
        tx_no.output[1].script_pubkey = ScriptBuf::new();
        assert!(process_fidelity(&tx_no).is_none());

        for hostile in [
            b"<script>#13460".as_slice(),
            b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa#abc".as_slice(),
            b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1#13460".as_slice(),
            b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa#013460".as_slice(),
        ] {
            assert!(process_fidelity(&fidelity_tx(lock, hostile)).is_none());
        }

        let mut too_small = fidelity_tx(lock, &valid_payload);
        too_small.output[0].value = Amount::from_sat(MIN_FIDELITY_BOND_AMOUNT_SATS - 1);
        assert!(process_fidelity(&too_small).is_none());

        let mut wrong_script = fidelity_tx(lock, &valid_payload);
        wrong_script.output[0].script_pubkey = ScriptBuf::new();
        assert!(process_fidelity(&wrong_script).is_none());

        let mut noncanonical = fidelity_tx(lock, &valid_payload);
        let mut pushdata = vec![0x6a, 0x4c, valid_payload.len() as u8];
        pushdata.extend_from_slice(&valid_payload);
        noncanonical.output[1].script_pubkey = pushdata.into();
        assert!(process_fidelity(&noncanonical).is_none());

        let too_soon = format!(
            "{}#{}",
            endpoint_for_test(),
            lock + MIN_FIDELITY_TIMELOCK - 1
        );
        assert!(process_fidelity(&fidelity_tx(lock, too_soon.as_bytes())).is_none());

        let too_late = format!(
            "{}#{}",
            endpoint_for_test(),
            lock + MAX_FIDELITY_TIMELOCK + 1
        );
        assert!(process_fidelity(&fidelity_tx(lock, too_late.as_bytes())).is_none());

        let mut final_sequence = fidelity_tx(lock, &valid_payload);
        final_sequence.input[0].sequence = Sequence::MAX;
        assert!(process_fidelity(&final_sequence).is_none());
    }

    fn endpoint_for_test() -> String {
        #[cfg(not(feature = "integration-test"))]
        return "a".repeat(56);
        #[cfg(feature = "integration-test")]
        return "127.0.0.1:9050".to_string();
    }

    #[test]
    fn test_process_transaction_in_block_false() {
        let mut reg = FileRegistry::new();

        let watched = OutPoint {
            txid: Txid::from_slice(&[3u8; 32]).unwrap(),
            vout: 1,
        };
        reg.upsert_watch(&WatchRequest {
            outpoint: watched,
            script_pubkey: ScriptBuf::new(),
            in_block: false,
            spent_tx: None,
        })
        .unwrap();

        let spending = tx(0, vec![watched], vec![]);
        process_transaction(&spending, &mut reg, false).unwrap();

        let w = reg.list_watches().unwrap().pop().unwrap();
        assert!(!w.in_block);
    }
    #[test]
    fn test_confirmed_spend_survives_a_mempool_rival() {
        let mut reg = FileRegistry::new();

        let watched = OutPoint {
            txid: Txid::from_slice(&[4u8; 32]).unwrap(),
            vout: 0,
        };
        // The confirmed spend is the one that may carry the preimage.
        let confirmed = tx(0, vec![watched], vec![ScriptBuf::new()]);
        reg.upsert_watch(&WatchRequest {
            outpoint: watched,
            script_pubkey: ScriptBuf::new(),
            in_block: true,
            spent_tx: Some(confirmed.clone()),
        })
        .unwrap();

        let rival = tx(0, vec![watched], vec![]);
        assert_ne!(rival.compute_txid(), confirmed.compute_txid());
        process_transaction(&rival, &mut reg, false).unwrap();

        let w = reg.list_watches().unwrap().pop().unwrap();
        assert_eq!(w.spent_tx, Some(confirmed));
        assert!(w.in_block);
    }

    #[test]
    fn test_confirmed_spend_replaces_a_mempool_spend() {
        let mut reg = FileRegistry::new();

        let watched = OutPoint {
            txid: Txid::from_slice(&[5u8; 32]).unwrap(),
            vout: 0,
        };
        let seen_first = tx(0, vec![watched], vec![ScriptBuf::new()]);
        reg.upsert_watch(&WatchRequest {
            outpoint: watched,
            script_pubkey: ScriptBuf::new(),
            in_block: false,
            spent_tx: Some(seen_first),
        })
        .unwrap();

        let mined = tx(0, vec![watched], vec![]);
        process_transaction(&mined, &mut reg, true).unwrap();

        let w = reg.list_watches().unwrap().pop().unwrap();
        assert_eq!(w.spent_tx, Some(mined));
        assert!(w.in_block);
    }

    #[test]
    fn test_seentxid_insert() {
        // 1. Insert new txid → returns true
        let mut seen_txid = SeenTxids::new();
        let txid1 = Txid::from_slice(&[0u8; 32]).unwrap();
        assert!(seen_txid.insert(txid1));

        // 2. Insert duplicate txid → returns false
        assert!(!seen_txid.insert(txid1));
        assert_eq!(seen_txid.order.len(), 1);

        // 3. Check for batch eviction when capacity exceeded
        for i in 0u64..(MAX_SEEN_TXIDS + 1) as u64 {
            let mut bytes = [0u8; 32];
            bytes[0..8].copy_from_slice(&i.to_be_bytes());
            let txid = Txid::from_slice(&bytes).unwrap();
            seen_txid.insert(txid);
        }
        assert_eq!(seen_txid.order.len(), MAX_SEEN_TXIDS - 10 + 1);
    }
}

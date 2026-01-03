//! Utility helpers for parsing watchtower-relevant transactions and updating registry state.

use std::str::FromStr;

use bitcoin::{
    absolute::{Height, LockTime},
    Block, Transaction, Txid,
};

use crate::watch_tower::{registry_storage::FileRegistry, watcher::Role};

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

    let (data_start, data_len) = match script.get(1)? {
        n @ 0x01..=0x4b => (2, *n as usize),
        0x4c => (3, *script.get(2)? as usize),
        0x4d => {
            let len = u16::from_le_bytes([*script.get(2)?, *script.get(3)?]) as usize;
            (4, len)
        }
        _ => return None,
    };

    script.get(data_start..data_start + data_len)
}

#[cfg(not(feature = "integration-test"))]
fn is_valid_onion_address(s: &str) -> bool {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 2 {
        return false;
    }
    let domain = parts[0];
    let port = parts[1];
    if !domain.ends_with(".onion") {
        return false;
    }
    matches!(port.parse::<u16>(), Ok(p) if p > 0)
}

#[cfg(feature = "integration-test")]
fn is_valid_address(s: &str) -> bool {
    use std::str::FromStr;

    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 2 {
        return false;
    }

    let ip = parts[0];
    let port = parts[1];

    if std::net::Ipv4Addr::from_str(ip).is_err() {
        return false;
    }

    matches!(port.parse::<u16>(), Ok(p) if p > 0)
}

fn parse_fidelity_op_return(data: &[u8]) -> Option<FidelityAnnouncement> {
    let decoded = std::str::from_utf8(data).ok()?;
    let (endpoint, locktime_str) = decoded.split_once('#')?;

    let expires_at_height = locktime_str.parse::<u32>().ok()?;

    #[cfg(not(feature = "integration-test"))]
    if !is_valid_onion_address(endpoint) {
        return None;
    }

    #[cfg(feature = "integration-test")]
    if !is_valid_address(endpoint) {
        return None;
    }

    Some(FidelityAnnouncement {
        onion: endpoint.to_string(),
        expires_at_height,
    })
}

/// Process a transaction for fidelity OP_RETURN announcement.
pub fn process_fidelity(tx: &Transaction) -> Option<FidelityAnnouncement> {
    // Fidelity txs must be timelocked
    if tx.lock_time == LockTime::Blocks(Height::ZERO) {
        return None;
    }

    // Expect bond + OP_RETURN (+ change)
    if !(2..=5).contains(&tx.output.len()) {
        return None;
    }

    for txout in &tx.output {
        if let Some(data) = extract_op_return_data(txout.script_pubkey.as_bytes()) {
            if let Some(ann) = parse_fidelity_op_return(data) {
                return Some(ann);
            }
        }
    }

    None
}

/// Processes each transaction in a block, updating watch entries and recording fidelity data.
pub fn process_block<R: Role>(block: Block, registry: &mut FileRegistry) {
    for tx in block.txdata.iter() {
        process_transaction(tx, registry, true);
        if R::RUN_DISCOVERY {
            let fidelity_announcement = process_fidelity(tx);
            if let Some(fidelity_announcement) = fidelity_announcement {
                let txid = tx.compute_txid();
                if registry.insert_fidelity(txid, fidelity_announcement) {
                    log::info!("Stored verified fidelity via blockchain: {}", txid);
                }
            }
        }
    }
}

/// Updates the registry for a transaction by clearing spent fidelities and marking watched spends.
pub fn process_transaction(tx: &Transaction, registry: &mut FileRegistry, in_block: bool) {
    let watch_requests = registry.list_watches();
    for input in &tx.input {
        let outpoint = input.previous_output;
        for watch_request in &watch_requests {
            if outpoint == watch_request.outpoint {
                let mut watch_request = watch_request.clone();
                watch_request.spent_tx = Some(tx.clone());
                watch_request.in_block = in_block;
                registry.upsert_watch(&watch_request);
            }
        }
    }
}

pub(crate) fn parse_fidelity_event(event: &nostr::Event) -> Option<(Txid, u32)> {
    let content = event.content.trim();
    let (txid, vout) = content.split_once(':')?;

    let txid = Txid::from_str(txid).ok()?;
    let vout = vout.parse::<u32>().ok()?;

    Some((txid, vout))
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
    use bitcoind::tempfile::TempDir;

    #[cfg(not(feature = "integration-test"))]
    const TEST_ADDR: &[u8] = b"aslkdfjbiakdsfn.onion:9050#500";
    #[cfg(feature = "integration-test")]
    const TEST_ADDR: &[u8] = b"127.0.0.1:9050#500";

    fn op_return(data: &[u8]) -> Vec<u8> {
        let mut script = vec![0x6a, data.len() as u8];
        script.extend_from_slice(data);
        script
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

    #[test]
    fn test_process_fidelity_valid() {
        let tx = tx(
            500,
            vec![OutPoint::null()],
            vec![ScriptBuf::new(), op_return(TEST_ADDR).into()],
        );

        let ann = process_fidelity(&tx).expect("expected valid fidelity announcement");

        #[cfg(not(feature = "integration-test"))]
        assert_eq!(ann.onion, "aslkdfjbiakdsfn.onion:9050");
        #[cfg(feature = "integration-test")]
        assert_eq!(ann.onion, "127.0.0.1:9050");
        assert_eq!(ann.expires_at_height, 500);
    }

    #[test]
    fn test_process_fidelity_invalid() {
        let tx0 = tx(
            0,
            vec![OutPoint::null()],
            vec![ScriptBuf::new(), op_return(TEST_ADDR).into()],
        );
        assert!(process_fidelity(&tx0).is_none());

        let tx1 = tx(1, vec![OutPoint::null()], vec![op_return(TEST_ADDR).into()]);
        assert!(process_fidelity(&tx1).is_none());

        let tx5 = tx(
            1,
            vec![OutPoint::null()],
            vec![
                ScriptBuf::new(),
                ScriptBuf::new(),
                ScriptBuf::new(),
                ScriptBuf::new(),
                ScriptBuf::new(),
                op_return(TEST_ADDR).into(),
            ],
        );
        assert!(process_fidelity(&tx5).is_none());

        let tx_no = tx(
            1,
            vec![OutPoint::null()],
            vec![ScriptBuf::new(), ScriptBuf::new()],
        );
        assert!(process_fidelity(&tx_no).is_none());

        let bad = op_return(b"aslkdfjbiakdsfn.onion:9050");
        let tx_bad = tx(
            1,
            vec![OutPoint::null()],
            vec![ScriptBuf::new(), bad.into()],
        );
        assert!(process_fidelity(&tx_bad).is_none());

        let bad2 = op_return(b"aslkdfjbiakdsfn.onion:9050#abc");
        let tx_bad2 = tx(
            1,
            vec![OutPoint::null()],
            vec![ScriptBuf::new(), bad2.into()],
        );
        assert!(process_fidelity(&tx_bad2).is_none());
    }

    #[test]
    fn test_process_transaction_in_block_false() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("reg.cbor");

        let mut reg = FileRegistry::load(&path);

        let watched = OutPoint {
            txid: Txid::from_slice(&[3u8; 32]).unwrap(),
            vout: 1,
        };
        reg.upsert_watch(&WatchRequest {
            outpoint: watched,
            in_block: false,
            spent_tx: None,
        });

        let spending = tx(0, vec![watched], vec![]);
        process_transaction(&spending, &mut reg, false);

        let w = reg.list_watches().pop().unwrap();
        assert!(!w.in_block);
    }
}

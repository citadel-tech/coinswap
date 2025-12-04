//! Utility helpers for parsing watchtower-relevant transactions and updating registry state.

use bitcoin::{
    absolute::{Height, LockTime},
    Block, Transaction,
};

use crate::watch_tower::registry_storage::FileRegistry;

/// Attempts to parse an OP_RETURN script and return a valid endpoint string.
/// Returns `None` if the script structure or endpoint is invalid.
pub fn extract_onion_address_from_script(script: &[u8]) -> Option<String> {
    if script.is_empty() || script[0] != 0x6a {
        return None;
    }
    if script.len() < 2 {
        return None;
    }
    let (data_start, data_len) = match script[1] {
        n @ 0x01..=0x4b => (2, n as usize),
        0x4c => {
            if script.len() < 3 {
                return None;
            }
            (3, script[2] as usize)
        }
        0x4d => {
            if script.len() < 4 {
                return None;
            }
            let len = u16::from_le_bytes([script[2], script[3]]) as usize;
            (4, len)
        }
        _ => {
            return None;
        }
    };
    if script.len() < data_start + data_len {
        return None;
    }

    let data = &script[data_start..data_start + data_len];
    let decoded = String::from_utf8(data.to_vec()).ok()?;

    #[cfg(not(feature = "integration-test"))]
    if is_valid_onion_address(&decoded) {
        Some(decoded)
    } else {
        None
    }

    #[cfg(feature = "integration-test")]
    if is_valid_address(&decoded) {
        Some(decoded)
    } else {
        None
    }
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

/// Scans a transaction for fidelity announcements and returns the onion endpoint if present.
pub fn process_fidelity(tx: &Transaction) -> Option<String> {
    if tx.lock_time == LockTime::Blocks(Height::ZERO) {
        return None;
    }
    if tx.output.len() < 2 || tx.output.len() > 5 {
        return None;
    }
    let onion_address = tx
        .output
        .iter()
        .find_map(|txout| extract_onion_address_from_script(txout.script_pubkey.as_bytes()));
    onion_address
}

/// Processes each transaction in a block, updating watch entries and recording fidelity data.
pub fn process_block(block: Block, registry: &mut FileRegistry) {
    for tx in block.txdata.iter() {
        process_transaction(tx, registry, true);
        let onion_address = process_fidelity(tx);
        if let Some(onion_address) = onion_address {
            registry.insert_fidelity(tx.compute_txid(), onion_address);
        }
    }
}

/// Updates the registry for a transaction by clearing spent fidelities and marking watched spends.
pub fn process_transaction(tx: &Transaction, registry: &mut FileRegistry, in_block: bool) {
    let fidelities = registry.list_fidelity();
    let watch_requests = registry.list_watches();

    for input in &tx.input {
        let outpoint = input.previous_output;
        for fidelity in &fidelities {
            if outpoint.txid == fidelity.txid {
                registry.remove_fidelity(outpoint.txid);
            }
        }
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
    const TEST_ADDR: &[u8] = b"aslkdfjbiakdsfn.onion:9050";
    #[cfg(feature = "integration-test")]
    const TEST_ADDR: &[u8] = b"127.0.0.1:9050";

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
    fn test_extract_onion_address_from_script_valid() {
        let script = op_return(TEST_ADDR);
        let expected = String::from_utf8(TEST_ADDR.to_vec()).unwrap();
        let result = extract_onion_address_from_script(&script)
            .expect("extract_onion_address_from_script_valid FAILED");
        assert_eq!(result, expected);
    }

    #[test]
    fn test_extract_onion_address_from_script_invalid() {
        //Not OP_RETURN
        assert!(extract_onion_address_from_script(&[0x51]).is_none());

        //Bad length
        let bad_len = vec![0x6a, 5, b'a', b'b'];
        assert!(extract_onion_address_from_script(&bad_len).is_none());

        //Wrong suffix (not .onion)
        let wrong_suffix = op_return(b"aslkdfjbiakdsfn.com:9050");
        assert!(extract_onion_address_from_script(&wrong_suffix).is_none());

        //Port zero
        let zero_port = op_return(b"aslkdfjbiakdsfn.onion:0");
        assert!(extract_onion_address_from_script(&zero_port).is_none());
    }

    #[test]
    fn test_process_fidelity_valid() {
        let txh = tx(
            1,
            vec![OutPoint::null()],
            vec![ScriptBuf::new(), op_return(TEST_ADDR).into()],
        );
        assert_eq!(
            process_fidelity(&txh),
            Some(String::from_utf8(TEST_ADDR.to_vec()).unwrap())
        );
    }

    #[test]
    fn test_process_fidelity_invalid() {
        //lock time zero
        let tx0 = tx(
            0,
            vec![OutPoint::null()],
            vec![ScriptBuf::new(), op_return(TEST_ADDR).into()],
        );
        assert!(process_fidelity(&tx0).is_none());

        //Transaction outputs length: too few
        let tx1 = tx(1, vec![OutPoint::null()], vec![op_return(TEST_ADDR).into()]);
        assert!(process_fidelity(&tx1).is_none());

        //Transaction outputs length: too many
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

        //No OP_RETURN (onion address)
        let tx_no = tx(
            1,
            vec![OutPoint::null()],
            vec![ScriptBuf::new(), ScriptBuf::new()],
        );
        assert!(process_fidelity(&tx_no).is_none());
    }

    #[test]
    fn test_process_transaction_mark_watch_and_removes_fidelity() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("reg.cbor");

        let mut reg = FileRegistry::load(&path);

        //Seed watcher and fidelity entry
        let watched = OutPoint {
            txid: Txid::from_slice(&[1u8; 32]).unwrap(),
            vout: 0,
        };
        reg.upsert_watch(&WatchRequest {
            outpoint: watched,
            in_block: false,
            spent_tx: None,
        });

        //insert fidelity
        let fid_txid = Txid::from_slice(&[2u8; 32]).unwrap();
        reg.insert_fidelity(fid_txid, String::from_utf8(TEST_ADDR.to_vec()).unwrap());

        let spending = tx(
            0,
            vec![
                OutPoint {
                    txid: fid_txid,
                    vout: 0,
                },
                watched,
            ],
            vec![],
        );
        let spend_txid = spending.compute_txid();

        process_transaction(&spending, &mut reg, true);

        //Fidelity removed check
        assert!(reg.list_fidelity().is_empty());

        //watches status check
        let w = reg.list_watches().pop().unwrap();
        assert!(w.in_block);
        assert_eq!(w.outpoint, watched);
        assert_eq!(w.spent_tx.unwrap().compute_txid(), spend_txid);
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

use bitcoin::{
    absolute::{Height, LockTime},
    Block, Transaction,
};

use crate::watch_tower::registry_storage::FileRegistry;

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

pub fn process_block(block: Block, registry: &mut FileRegistry) {
    for tx in block.txdata.iter() {
        process_transaction(tx, registry, true);
        let onion_address = process_fidelity(tx);
        if let Some(onion_address) = onion_address {
            registry.insert_fidelity(tx.compute_txid(), onion_address);
        }
    }
}

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

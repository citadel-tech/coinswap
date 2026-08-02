//! Electrum over a real Tor SOCKS5 proxy, under the most adverse abort paths.
//!
//! These reuse existing scenario bodies verbatim via [`TorElectrumBackend`], so
//! they assert the *same* balances as their non-Tor counterparts. A Tor-specific
//! divergence in any watchtower path therefore fails loudly instead of degrading
//! silently.
//!
//! Why these three scenarios, in terms of watchtower coverage:
//!
//! - `abort1` — the taker vanishes *after* funding is on-chain, so makers must
//!   detect it autonomously with no ZMQ and no mempool scan: `subscribe_script` /
//!   `poll_event`, the preimage/hashlock cascade, timelock fallback, and
//!   `unsubscribe_script` on cleanup. Run over both protocols.
//! - `reboot_recovery` — the only scenario that restarts a maker mid-swap, hence
//!   the only one exercising the watcher's startup re-subscribe of persisted
//!   watches, which is the path most exposed to Tor circuit churn.
//! - `malice2` — the only scenario driving the taker's breach detector.
//!
//! ## Requirements and gating
//!
//! An ephemeral onion service must upload its descriptor to the HSDir ring and
//! the client must fetch it back, so these **cannot work offline**. They are
//! `#[ignore]`d and additionally require `COINSWAP_TOR_IT=1`, and they *skip*
//! rather than fail when Tor is missing. Run them with:
//!
//! ```text
//! COINSWAP_TOR_IT=1 cargo test --features integration-test electrum_tor \
//!     -- --ignored --test-threads=1 --nocapture
//! ```
//!
//! `tor` must be listening on `TOR_CONTROL_PORT` / `TOR_SOCKS_PORT` and be fully
//! bootstrapped. Set `COINSWAP_TOR_PASSWORD` if the control port needs one.
//!
//! Ignored does not mean dead: no hermetic suite can reach the Tor network, so
//! these stay opt-in by design. They are kept because `tor_reboot_recovery` is
//! the only coverage of the watcher's re-subscribe-on-reboot path, and they run
//! before every release.
//!
//! Note the onion services are created with `Flags=Detach`, so they outlive the
//! test process. That is deliberate — the CI job's tor is ephemeral and drops
//! them on restart.

use coinswap::protocol::common_messages::ProtocolVersion;

use super::{
    electrum_abort1::{run_abort1, LEGACY_EXPECTED, TAPROOT_EXPECTED},
    malice2::run_malice2,
    taproot_reboot_recovery::run_reboot_recovery,
    test_framework::{tor_it_enabled, TorElectrumBackend},
};

use log::warn;

/// Taproot abort1 over Tor: the widest watchtower path in the suite.
#[test]
#[ignore = "requires a bootstrapped tor and COINSWAP_TOR_IT=1"]
fn tor_abort1_taproot() {
    if !tor_it_enabled() {
        return;
    }
    warn!("Running Test: abort1 (Taproot) over Tor Electrum");
    run_abort1::<TorElectrumBackend>(ProtocolVersion::Taproot, &TAPROOT_EXPECTED);
}

/// Legacy abort1 over Tor. Same cascade, different contract shape.
#[test]
#[ignore = "requires a bootstrapped tor and COINSWAP_TOR_IT=1"]
fn tor_abort1_legacy() {
    if !tor_it_enabled() {
        return;
    }
    warn!("Running Test: abort1 (Legacy) over Tor Electrum");
    run_abort1::<TorElectrumBackend>(ProtocolVersion::Legacy, &LEGACY_EXPECTED);
}

/// Maker reboot mid-swap over Tor.
///
/// The restarted maker rebuilds its `Electrum` from the same config, so it
/// reconnects to the same (detached) onion service and must re-subscribe every
/// persisted watch. This is the only test covering that path on any backend.
#[test]
#[ignore = "requires a bootstrapped tor and COINSWAP_TOR_IT=1"]
fn tor_reboot_recovery() {
    if !tor_it_enabled() {
        return;
    }
    warn!("Running Test: maker reboot recovery over Tor Electrum");
    run_reboot_recovery::<TorElectrumBackend>();
}

/// Malicious contract broadcast over Tor, exercising the taker's breach detector.
#[test]
#[ignore = "requires a bootstrapped tor and COINSWAP_TOR_IT=1"]
fn tor_malice2() {
    if !tor_it_enabled() {
        return;
    }
    warn!("Running Test: malice2 (maker broadcasts contract) over Tor Electrum");
    run_malice2::<TorElectrumBackend>();
}

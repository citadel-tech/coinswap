//! Blockchain backend abstraction shared by the wallet and the watchtower.
//!
//! A single [`Blockchain`] trait covers every chain operation either component
//! needs — UTXO/transaction queries, broadcasting, descriptor import and rescan,
//! script tracking, and watchtower notifications. Two concrete backends implement
//! it: [`CoreRPC`] (Bitcoin Core over JSON-RPC + ZMQ) and [`Electrum`] (the
//! Electrum protocol). [`AnyBlockchain`] is a runtime selector enum that also
//! implements [`Blockchain`], so the wallet holds a single
//! `blockchain: AnyBlockchain` and the whole stack stays non-generic while the
//! backend is still chosen at runtime.
//!
//! All methods take `&self`: the concrete backends use interior mutability for
//! their caches and notification state, which keeps every backend `Send + Sync`
//! so the wallet can be shared as `Arc<RwLock<Wallet>>`.
//!
//! Think of this module as the thin scaffolding that lets the rest of the code
//! talk to "a blockchain" without caring whether that is Bitcoin Core or an
//! Electrum server.

mod corerpc;
mod electrum;

pub use corerpc::{CoreRPC, CoreRpcConfig};
pub use electrum::{Electrum, ElectrumConfig};

use std::fmt::Debug;

use bitcoin::{
    address::NetworkUnchecked, block::Header, Address, BlockHash, Script, Transaction, Txid,
};
use bitcoind::bitcoincore_rpc::json::{
    EstimateMode, EstimateSmartFeeResult, GetAddressInfoResult, GetBlockHeaderResult,
    GetBlockchainInfoResult, GetRawTransactionResult, GetTxOutResult, ListTransactionResult,
    ListUnspentResultEntry, ScanningDetails,
};
use serde_json::Value;

use super::error::WalletError;

/// Error for a [`Blockchain`] method the active backend does not support
/// (e.g. SPV proofs or `estimatesmartfee` on Electrum).
fn unsupported(method: &str) -> WalletError {
    WalletError::General(format!("{method} is not supported by this backend"))
}

/// HD-origin metadata for a watched script.
///
/// Returned by [`Blockchain::hd_origin_for_script`] so the wallet's UTXO
/// classifier can recognise an Electrum-sourced UTXO as a `SeedCoin`/`SweptCoin`
/// without a descriptor round-trip (Bitcoin Core carries the same info in the
/// descriptor string on each UTXO and returns `None` here).
#[derive(Debug, Clone)]
pub struct HdOrigin {
    /// Fingerprint of the account-level xpub the script was derived under.
    pub fingerprint: String,
    /// 0 for external (receive) chain, 1 for internal (change) chain.
    pub keychain_idx: u32,
    /// Index along the chosen chain.
    pub index: u32,
    /// True if derived under the P2TR descriptor, false for P2WPKH.
    pub is_taproot: bool,
}

/// Reference to a block surfaced by [`Blockchain::poll_event`].
///
/// Used by the watchtower to checkpoint the chain tip. The Bitcoin Core backend
/// ships full serialized block bytes in `hash`; the Electrum backend ships just
/// the 32-byte block hash (with `height` populated).
#[derive(Debug, Clone)]
pub struct BlockRef {
    /// Height of the block if known, `0` when unavailable.
    pub height: u64,
    /// Either the full serialized block (Core/ZMQ) or the 32-byte hash (Electrum).
    pub hash: Vec<u8>,
}

/// Events emitted by [`Blockchain::poll_event`], consumed by the watchtower loop.
#[derive(Debug, Clone)]
pub enum WatchEvent {
    /// A transaction was seen (mempool or block); carries its raw bytes.
    TxSeen {
        /// Raw transaction bytes.
        raw_tx: Vec<u8>,
    },
    /// The chain tip advanced; carries the new block reference.
    BlockConnected(BlockRef),
}

/// Resolved backend selector. Built from the user's init config (the single
/// source of truth) and handed to [`AnyBlockchain::from_config`] and the
/// watchtower service.
#[derive(Debug, Clone)]
pub enum BackendConfig {
    /// Drive RPC through Bitcoin Core.
    CoreRpc(CoreRpcConfig),
    /// Drive RPC through an Electrum-protocol server.
    Electrum(ElectrumConfig),
}

/// Backend abstraction over a blockchain data source.
///
/// Implemented by [`CoreRPC`], [`Electrum`], and [`AnyBlockchain`]. The wallet
/// holds an instance for its UTXO/sync operations; the watchtower holds its own
/// instance for queries and notifications. Methods that only make sense for one
/// backend (descriptor import, SPV proofs, watch-only wallet bootstrap) provide
/// defaults or return errors on the other.
pub trait Blockchain: Send + Sync + 'static {
    /// True only for the Electrum backend; selects the wallet's sync path.
    fn is_electrum(&self) -> bool {
        false
    }

    // ---- chain / transaction queries -----------------------------------

    /// Chain metadata (`chain`, `best_block_hash`, …). Used for network
    /// detection at wallet init and fidelity-bond tip lookups.
    fn get_blockchain_info(&self) -> Result<GetBlockchainInfoResult, WalletError>;
    /// Current chain tip height.
    fn get_block_count(&self) -> Result<u64, WalletError>;
    /// Block hash at `height`.
    fn get_block_hash(&self, height: u64) -> Result<BlockHash, WalletError>;
    /// Block header for `hash`.
    fn get_block_header(&self, hash: &BlockHash) -> Result<Header, WalletError>;
    /// Verbose block header (incl. confirmations); used by fidelity locktime logic.
    fn get_block_header_info(&self, hash: &BlockHash) -> Result<GetBlockHeaderResult, WalletError>;
    /// Fetch a raw transaction by txid.
    fn get_raw_transaction(
        &self,
        txid: &Txid,
        block_hash: Option<&BlockHash>,
    ) -> Result<Transaction, WalletError>;
    /// Verbose transaction info; consumers read `confirmations`.
    fn get_raw_transaction_info(
        &self,
        txid: &Txid,
        block_hash: Option<&BlockHash>,
    ) -> Result<GetRawTransactionResult, WalletError>;
    /// UTXO at `txid:vout`, or `None` once spent. The `None` transition is
    /// load-bearing for recovery and funding-confirmation logic.
    fn get_tx_out(
        &self,
        txid: &Txid,
        vout: u32,
        include_mempool: Option<bool>,
    ) -> Result<Option<GetTxOutResult>, WalletError>;
    /// List wallet UTXOs with at least `minconf` (and at most `maxconf`) confirmations.
    fn list_unspent(
        &self,
        minconf: Option<usize>,
        maxconf: Option<usize>,
    ) -> Result<Vec<ListUnspentResultEntry>, WalletError>;
    /// Broadcast a fully-signed transaction.
    fn send_raw_transaction(&self, tx: &Transaction) -> Result<Txid, WalletError>;
    /// Expand a descriptor to addresses over an optional `[start, end]` range.
    fn derive_addresses(
        &self,
        descriptor: &str,
        range: Option<[u32; 2]>,
    ) -> Result<Vec<Address<NetworkUnchecked>>, WalletError>;
    /// Recent wallet transactions (Core only; used by the FFI history view).
    /// Defaults to an error on backends without a server-side wallet (Electrum).
    fn list_transactions(
        &self,
        _label: Option<&str>,
        _count: Option<usize>,
        _skip: Option<usize>,
        _include_watchonly: Option<bool>,
    ) -> Result<Vec<ListTransactionResult>, WalletError> {
        Err(unsupported("list_transactions"))
    }
    /// `estimatesmartfee` for `conf_target` blocks. Defaults to an error on
    /// Electrum (callers fall back to the mempool.space/esplora estimators).
    fn estimate_smart_fee(
        &self,
        _conf_target: u16,
        _estimate_mode: Option<EstimateMode>,
    ) -> Result<EstimateSmartFeeResult, WalletError> {
        Err(unsupported("estimate_smart_fee"))
    }
    // ---- Core-only sync helpers ----------------------------------------
    // These are invoked by `Wallet::sync` only on the Bitcoin Core path
    // (after an early return for Electrum), so the Electrum stubs are never hit.

    /// Bootstrap the watch-only wallet on the node (load or create, version-gated).
    /// No-op on Electrum.
    fn prepare_backend_wallet(&self, _wallet_name: &str) -> Result<(), WalletError> {
        Ok(())
    }
    /// Address metadata used to detect already-imported descriptors. Defaults to
    /// an error on Electrum (never reached: the sync path early-returns first).
    fn get_address_info(&self, _addr: &Address) -> Result<GetAddressInfoResult, WalletError> {
        Err(unsupported("get_address_info"))
    }
    /// Import descriptor request objects and trigger a rescan. Defaults to a
    /// no-op (Electrum pre-derives and registers scripts via `watch_script`).
    fn import_descriptors(&self, _requests: &[Value]) -> Result<(), WalletError> {
        Ok(())
    }
    /// Current rescan status (`getwalletinfo.scanning`); `None` when not scanning.
    /// Defaults to `None` (Electrum never rescans).
    fn wallet_scanning_status(&self) -> Result<Option<ScanningDetails>, WalletError> {
        Ok(None)
    }

    // ---- script tracking -----------------------------------------------

    /// Register a scriptPubKey for UTXO lookups. No-op on Bitcoin Core (the
    /// server-side wallet tracks these); Electrum stores it in a local watch set
    /// with an optional HD-origin hint.
    fn watch_script(&self, _script: &Script, _hd: Option<HdOrigin>) {}
    /// HD-origin recorded for `script` (Electrum only; Core returns `None`).
    fn hd_origin_for_script(&self, _script: &Script) -> Option<HdOrigin> {
        None
    }

    // ---- watchtower notifications --------------------------------------

    /// Non-blocking poll for the next chain event (new tx or connected block).
    /// Lazily establishes the notification channel (ZMQ socket / Electrum
    /// header subscription) on first call.
    fn poll_event(&self) -> Option<WatchEvent>;
    /// Arm a per-script notification so future activity surfaces via
    /// [`poll_event`](Self::poll_event). No-op on Bitcoin Core (the `rawtx`
    /// feed already sees everything).
    fn subscribe_script(&self, _spk: &Script) -> Result<(), WalletError> {
        Ok(())
    }
    /// Drop a previously-armed per-script subscription. No-op on Bitcoin Core.
    fn unsubscribe_script(&self, _spk: &Script) -> Result<(), WalletError> {
        Ok(())
    }
    /// Txids currently in the node mempool (Core only; Electrum returns empty,
    /// which makes the watchtower skip its startup mempool scan).
    fn get_raw_mempool(&self) -> Result<Vec<Txid>, WalletError> {
        Ok(Vec::new())
    }
    /// Bitcoin chain name string: `"main"`/`"test"`/`"signet"`/`"regtest"`.
    fn chain_name(&self) -> Result<String, WalletError>;
}

/// Runtime backend selector. Implements [`Blockchain`] by dispatching to the
/// active variant, so top-level `Taker`/`MakerServer` init stays non-generic
/// while still choosing the backend from user config at runtime.
//
// The variants differ in size (Electrum carries several local caches), but only
// a handful of instances ever exist per process, so the size difference is not
// worth an extra heap indirection on every backend call.
#[allow(clippy::large_enum_variant)]
pub enum AnyBlockchain {
    /// Bitcoin Core (JSON-RPC + ZMQ).
    CoreRPC(CoreRPC),
    /// Electrum protocol.
    Electrum(Electrum),
}

impl AnyBlockchain {
    /// Build the active backend from the resolved [`BackendConfig`]. Each
    /// consumer (wallet, watchtower watcher, discovery) builds its own instance.
    pub fn from_config(backend: &BackendConfig) -> Result<Self, WalletError> {
        match backend {
            BackendConfig::CoreRpc(cfg) => Ok(AnyBlockchain::CoreRPC(CoreRPC::new(cfg)?)),
            BackendConfig::Electrum(cfg) => Ok(AnyBlockchain::Electrum(Electrum::new(cfg)?)),
        }
    }

    /// Open a fresh, independent connection to the same backend.
    ///
    /// Lets a second consumer (e.g. the watchtower discovery thread) get its own
    /// live connection by rebuilding from the backend's own stored config, so
    /// neither the wallet nor the watchtower has to keep a separate
    /// [`BackendConfig`] around.
    pub fn new_connection(&self) -> Result<Self, WalletError> {
        match self {
            AnyBlockchain::CoreRPC(b) => Ok(AnyBlockchain::CoreRPC(b.reconnect()?)),
            AnyBlockchain::Electrum(b) => Ok(AnyBlockchain::Electrum(b.reconnect()?)),
        }
    }
}

/// Dispatch each [`Blockchain`] method to the active backend variant.
impl Blockchain for AnyBlockchain {
    fn is_electrum(&self) -> bool {
        match self {
            AnyBlockchain::CoreRPC(b) => b.is_electrum(),
            AnyBlockchain::Electrum(b) => b.is_electrum(),
        }
    }
    fn get_blockchain_info(&self) -> Result<GetBlockchainInfoResult, WalletError> {
        match self {
            AnyBlockchain::CoreRPC(b) => b.get_blockchain_info(),
            AnyBlockchain::Electrum(b) => b.get_blockchain_info(),
        }
    }
    fn get_block_count(&self) -> Result<u64, WalletError> {
        match self {
            AnyBlockchain::CoreRPC(b) => b.get_block_count(),
            AnyBlockchain::Electrum(b) => b.get_block_count(),
        }
    }
    fn get_block_hash(&self, height: u64) -> Result<BlockHash, WalletError> {
        match self {
            AnyBlockchain::CoreRPC(b) => b.get_block_hash(height),
            AnyBlockchain::Electrum(b) => b.get_block_hash(height),
        }
    }
    fn get_block_header(&self, hash: &BlockHash) -> Result<Header, WalletError> {
        match self {
            AnyBlockchain::CoreRPC(b) => b.get_block_header(hash),
            AnyBlockchain::Electrum(b) => b.get_block_header(hash),
        }
    }
    fn get_block_header_info(&self, hash: &BlockHash) -> Result<GetBlockHeaderResult, WalletError> {
        match self {
            AnyBlockchain::CoreRPC(b) => b.get_block_header_info(hash),
            AnyBlockchain::Electrum(b) => b.get_block_header_info(hash),
        }
    }
    fn get_raw_transaction(
        &self,
        txid: &Txid,
        block_hash: Option<&BlockHash>,
    ) -> Result<Transaction, WalletError> {
        match self {
            AnyBlockchain::CoreRPC(b) => b.get_raw_transaction(txid, block_hash),
            AnyBlockchain::Electrum(b) => b.get_raw_transaction(txid, block_hash),
        }
    }
    fn get_raw_transaction_info(
        &self,
        txid: &Txid,
        block_hash: Option<&BlockHash>,
    ) -> Result<GetRawTransactionResult, WalletError> {
        match self {
            AnyBlockchain::CoreRPC(b) => b.get_raw_transaction_info(txid, block_hash),
            AnyBlockchain::Electrum(b) => b.get_raw_transaction_info(txid, block_hash),
        }
    }
    fn get_tx_out(
        &self,
        txid: &Txid,
        vout: u32,
        include_mempool: Option<bool>,
    ) -> Result<Option<GetTxOutResult>, WalletError> {
        match self {
            AnyBlockchain::CoreRPC(b) => b.get_tx_out(txid, vout, include_mempool),
            AnyBlockchain::Electrum(b) => b.get_tx_out(txid, vout, include_mempool),
        }
    }
    fn list_unspent(
        &self,
        minconf: Option<usize>,
        maxconf: Option<usize>,
    ) -> Result<Vec<ListUnspentResultEntry>, WalletError> {
        match self {
            AnyBlockchain::CoreRPC(b) => b.list_unspent(minconf, maxconf),
            AnyBlockchain::Electrum(b) => b.list_unspent(minconf, maxconf),
        }
    }
    fn send_raw_transaction(&self, tx: &Transaction) -> Result<Txid, WalletError> {
        match self {
            AnyBlockchain::CoreRPC(b) => b.send_raw_transaction(tx),
            AnyBlockchain::Electrum(b) => b.send_raw_transaction(tx),
        }
    }
    fn derive_addresses(
        &self,
        descriptor: &str,
        range: Option<[u32; 2]>,
    ) -> Result<Vec<Address<NetworkUnchecked>>, WalletError> {
        match self {
            AnyBlockchain::CoreRPC(b) => b.derive_addresses(descriptor, range),
            AnyBlockchain::Electrum(b) => b.derive_addresses(descriptor, range),
        }
    }
    fn list_transactions(
        &self,
        label: Option<&str>,
        count: Option<usize>,
        skip: Option<usize>,
        include_watchonly: Option<bool>,
    ) -> Result<Vec<ListTransactionResult>, WalletError> {
        match self {
            AnyBlockchain::CoreRPC(b) => b.list_transactions(label, count, skip, include_watchonly),
            AnyBlockchain::Electrum(b) => {
                b.list_transactions(label, count, skip, include_watchonly)
            }
        }
    }
    fn estimate_smart_fee(
        &self,
        conf_target: u16,
        estimate_mode: Option<EstimateMode>,
    ) -> Result<EstimateSmartFeeResult, WalletError> {
        match self {
            AnyBlockchain::CoreRPC(b) => b.estimate_smart_fee(conf_target, estimate_mode),
            AnyBlockchain::Electrum(b) => b.estimate_smart_fee(conf_target, estimate_mode),
        }
    }
    fn prepare_backend_wallet(&self, wallet_name: &str) -> Result<(), WalletError> {
        match self {
            AnyBlockchain::CoreRPC(b) => b.prepare_backend_wallet(wallet_name),
            AnyBlockchain::Electrum(b) => b.prepare_backend_wallet(wallet_name),
        }
    }
    fn get_address_info(&self, addr: &Address) -> Result<GetAddressInfoResult, WalletError> {
        match self {
            AnyBlockchain::CoreRPC(b) => b.get_address_info(addr),
            AnyBlockchain::Electrum(b) => b.get_address_info(addr),
        }
    }
    fn import_descriptors(&self, requests: &[Value]) -> Result<(), WalletError> {
        match self {
            AnyBlockchain::CoreRPC(b) => b.import_descriptors(requests),
            AnyBlockchain::Electrum(b) => b.import_descriptors(requests),
        }
    }
    fn wallet_scanning_status(&self) -> Result<Option<ScanningDetails>, WalletError> {
        match self {
            AnyBlockchain::CoreRPC(b) => b.wallet_scanning_status(),
            AnyBlockchain::Electrum(b) => b.wallet_scanning_status(),
        }
    }
    fn watch_script(&self, script: &Script, hd: Option<HdOrigin>) {
        match self {
            AnyBlockchain::CoreRPC(b) => b.watch_script(script, hd),
            AnyBlockchain::Electrum(b) => b.watch_script(script, hd),
        }
    }
    fn hd_origin_for_script(&self, script: &Script) -> Option<HdOrigin> {
        match self {
            AnyBlockchain::CoreRPC(b) => b.hd_origin_for_script(script),
            AnyBlockchain::Electrum(b) => b.hd_origin_for_script(script),
        }
    }
    fn poll_event(&self) -> Option<WatchEvent> {
        match self {
            AnyBlockchain::CoreRPC(b) => b.poll_event(),
            AnyBlockchain::Electrum(b) => b.poll_event(),
        }
    }
    fn subscribe_script(&self, spk: &Script) -> Result<(), WalletError> {
        match self {
            AnyBlockchain::CoreRPC(b) => b.subscribe_script(spk),
            AnyBlockchain::Electrum(b) => b.subscribe_script(spk),
        }
    }
    fn unsubscribe_script(&self, spk: &Script) -> Result<(), WalletError> {
        match self {
            AnyBlockchain::CoreRPC(b) => b.unsubscribe_script(spk),
            AnyBlockchain::Electrum(b) => b.unsubscribe_script(spk),
        }
    }
    fn get_raw_mempool(&self) -> Result<Vec<Txid>, WalletError> {
        match self {
            AnyBlockchain::CoreRPC(b) => b.get_raw_mempool(),
            AnyBlockchain::Electrum(b) => b.get_raw_mempool(),
        }
    }
    fn chain_name(&self) -> Result<String, WalletError> {
        match self {
            AnyBlockchain::CoreRPC(b) => b.chain_name(),
            AnyBlockchain::Electrum(b) => b.chain_name(),
        }
    }
}

/// Match an Electrum server's `server_features().genesis_hash` against the known
/// network genesis blocks. Used by [`Electrum::new`] and `chain_name` to detect
/// the network the server is serving.
pub fn network_from_electrum_genesis(genesis: &[u8]) -> Option<bitcoin::Network> {
    use bitcoin::hashes::Hash;
    for net in [
        bitcoin::Network::Bitcoin,
        bitcoin::Network::Testnet,
        bitcoin::Network::Signet,
        bitcoin::Network::Regtest,
    ] {
        let mut local = bitcoin::constants::genesis_block(net)
            .block_hash()
            .to_raw_hash()
            .to_byte_array();
        // Electrum returns `genesis_hash` in display (big-endian) byte order, while
        // `to_byte_array()` returns internal little-endian bytes, hence the reverse.
        local.reverse();
        if local == genesis {
            return Some(net);
        }
    }
    None
}

/// Bitcoin Core's chain-name string for a network (`"main"`/`"test"`/`"signet"`/`"regtest"`).
pub fn chain_name_for(net: bitcoin::Network) -> &'static str {
    match net {
        bitcoin::Network::Bitcoin => "main",
        bitcoin::Network::Testnet => "test",
        bitcoin::Network::Signet => "signet",
        bitcoin::Network::Regtest => "regtest",
        _ => "unknown",
    }
}

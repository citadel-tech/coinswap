//! Backend abstraction over a blockchain RPC source.
//!
//! Defines the [`BlockchainBackend`] trait and the [`BackendConfig`] selector,
//! with the Bitcoin Core backend ([`BitcoindBackend`], an alias for
//! `bitcoincore_rpc::Client`) implemented here. The Electrum backend lives in
//! [`super::electrum`].
use std::{fmt::Debug, thread};

use bitcoind::bitcoincore_rpc::{
    json::{ListUnspentResultEntry, ScanningDetails},
    Auth, RpcApi,
};
use serde_json::{json, Value};

use crate::{utill::HEART_BEAT_INTERVAL, wallet::api::KeychainKind};

use bitcoin::{block::Header, Script};
use serde::Deserialize;

use super::{electrum::ElectrumConfig, error::WalletError, Wallet};

/// Bitcoin Core JSON-RPC backend
pub type BitcoindBackend = bitcoind::bitcoincore_rpc::Client;

/// Backend abstraction over a blockchain RPC source. Inherits [`RpcApi`] so wallet calls stay uniform.
pub trait BlockchainBackend: RpcApi + Debug + Send + Sync + 'static {
    /// Per-backend connection config (e.g. [`RPCConfig`], [`ElectrumConfig`]).
    type Config: Clone;
    /// True only for [`ElectrumBackend`]. Selects the sync implementation.
    const IS_ELECTRUM: bool = false;

    /// Construct the backend client from its config.
    fn from_config(config: &Self::Config) -> Result<Self, WalletError>
    where
        Self: Sized;

    /// Borrow the matching variant out of a [`BackendConfig`].
    fn from_backend_config(backend: &BackendConfig) -> Result<&Self::Config, WalletError>;

    /// Register a scriptPubKey for UTXO lookups. No-op on Bitcoin Core; Electrum  stores it locally along with an optional HD-origin hint.
    fn watch_script(&self, _script: &Script, _hd: Option<HdOrigin>) {}

    /// HD-origin recorded for `script_pubkey` (Electrum only). Bitcoin Core exposes the same info via the descriptor string on each UTXO, so the
    /// default returns `None`. Used by the wallet's UTXO classifier to recognise Electrum-sourced seed coins without round-tripping through a descriptor.
    fn hd_origin_for_script(&self, _script: &Script) -> Option<HdOrigin> {
        None
    }
}

impl BlockchainBackend for BitcoindBackend {
    type Config = RPCConfig;
    fn from_config(c: &RPCConfig) -> Result<Self, WalletError> {
        Ok(BitcoindBackend::new(
            &format!("http://{}/wallet/{}", c.url, c.wallet_name),
            c.auth.clone(),
        )?)
    }
    fn from_backend_config(b: &BackendConfig) -> Result<&RPCConfig, WalletError> {
        match b {
            BackendConfig::Bitcoind(c) => Ok(c),
            BackendConfig::Electrum(_) => Err(WalletError::General(
                "expected Bitcoind, got Electrum".into(),
            )),
        }
    }
}

impl BackendConfig {
    /// Borrow the wallet name from whichever variant is set.
    pub fn wallet_name(&self) -> &str {
        match self {
            BackendConfig::Bitcoind(c) => &c.wallet_name,
            BackendConfig::Electrum(c) => &c.wallet_name,
        }
    }

    /// Overwrite the wallet name on whichever variant is set. Used by
    /// interactive restore where the wallet name is derived from the restored path.
    pub fn set_wallet_name(&mut self, name: String) {
        match self {
            BackendConfig::Bitcoind(c) => c.wallet_name = name,
            BackendConfig::Electrum(c) => c.wallet_name = name,
        }
    }
}

/// HD-origin metadata for a watched script. Returned by
/// [`BlockchainBackend::hd_origin_for_script`] so the wallet's UTXO classifier
/// can identify an Electrum-sourced UTXO as a `SeedCoin` (or `SweptCoin`).
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

/// Top-level backend selection consumed by binaries / FFI.
#[derive(Debug, Clone)]
pub enum BackendConfig {
    /// Drive RPC through Bitcoin Core.
    Bitcoind(RPCConfig),
    /// Drive RPC through an Electrum-protocol server.
    Electrum(ElectrumConfig),
}

/// Configuration parameters for connecting to a Bitcoin node via RPC.
#[derive(Debug, Clone)]
pub struct RPCConfig {
    /// The bitcoin node url
    pub url: String,
    /// The bitcoin node authentication mechanism
    pub auth: Auth,
    /// The wallet name in the bitcoin node, derive this from the descriptor.
    pub wallet_name: String,
    /// ZMQ endpoint for block/tx notifications (e.g. `"tcp://127.0.0.1:28332"`).
    /// Only used by components that need push notifications (maker watch service).
    pub zmq_addr: String,
}

const RPC_HOSTPORT: &str = "localhost:18443";

impl Default for RPCConfig {
    fn default() -> Self {
        Self {
            url: RPC_HOSTPORT.to_string(),
            auth: Auth::UserPass("regtestrpcuser".to_string(), "regtestrpcpass".to_string()),
            wallet_name: "random-wallet-name".to_string(),
            zmq_addr: "tcp://127.0.0.1:28332".to_string(),
        }
    }
}

fn list_wallet_dir<B: RpcApi>(client: &B) -> Result<Vec<String>, WalletError> {
    #[derive(Deserialize)]
    struct Name {
        name: String,
    }
    #[derive(Deserialize)]
    struct CallResult {
        wallets: Vec<Name>,
    }

    let result: CallResult = client.call("listwalletdir", &[])?;
    Ok(result.wallets.into_iter().map(|n| n.name).collect())
}

fn get_wallet_scanning_details<B: RpcApi>(
    client: &B,
) -> Result<Option<ScanningDetails>, WalletError> {
    #[derive(Deserialize)]
    struct WalletInfoScanningOnly {
        scanning: Option<ScanningDetails>,
    }

    // Parse only the field we need so upstream schema removals (e.g. getwalletinfo v30 balance related fields removal)
    // do not break deserialization.
    let wallet_info: WalletInfoScanningOnly = client.call("getwalletinfo", &[])?;
    Ok(wallet_info.scanning)
}

impl<B: BlockchainBackend> Wallet<B> {
    /// Sync the wallet, then persist to disk.
    pub fn sync_and_save(&mut self) -> Result<(), WalletError> {
        log::info!("Sync Started for {:?}", &self.store.file_name);
        self.sync_no_fail();
        self.save_to_disk()?;
        log::info!("Synced & Saved {:?}", &self.store.file_name);
        Ok(())
    }

    /// Get all utxos tracked by the core rpc wallet.
    fn get_all_utxo_from_rpc(&self) -> Result<Vec<ListUnspentResultEntry>, WalletError> {
        self.rpc.unlock_unspent_all()?;
        let all_utxos = self
            .rpc
            .list_unspent(Some(0), Some(9999999), None, None, None)?;
        Ok(all_utxos)
    }

    /// Bitcoin Core's importdescriptors + scan vs Electrum's walks scripthhash history.
    fn sync(&mut self) -> Result<(), WalletError> {
        if B::IS_ELECTRUM {
            return self.sync_no_rescan();
        }
        // Create or load the watch-only bitcoin core wallet
        let wallet_name = &self.store.file_name;
        if self.rpc.list_wallets()?.contains(wallet_name) {
            log::debug!("wallet already loaded: {wallet_name}");
        } else if list_wallet_dir(&self.rpc)?.contains(wallet_name) {
            self.rpc.load_wallet(wallet_name)?;
            log::debug!("wallet loaded: {wallet_name}");
        } else {
            // pre-0.21 use legacy wallets
            if self.rpc.version()? < 210_000 {
                self.rpc
                    .create_wallet(wallet_name, Some(true), None, None, None)?;
            } else {
                // We cannot use the api directly right now.
                // https://github.com/rust-bitcoin/rust-bitcoincore-rpc/issues/225 is still open,
                // We can update to api call after moving to new corepc crate.
                let args = [
                    Value::String(wallet_name.clone()),
                    Value::Bool(true),  // Disable Private Keys
                    Value::Bool(false), // Create a blank wallet
                    Value::Null,        // Optional Passphrase
                    Value::Bool(false), // Avoid Reuse
                    Value::Bool(true),  // Descriptor Wallet
                ];
                let _: Value = self.rpc.call("createwallet", &args)?;
            }

            log::debug!("wallet created: {wallet_name}");
        }

        let descriptors_to_import = self.descriptors_to_import()?;

        if descriptors_to_import.is_empty() {
            return Ok(());
        }

        // Sometimes in test multiple wallet scans can occur at same time, resulting in error.
        let mut last_synced_height = self
            .store
            .last_synced_height
            .unwrap_or(0)
            .max(self.store.wallet_birthday.unwrap_or(0));
        let node_synced = self.rpc.get_block_count()?;

        // If the chain is shorter than the wallet's last synced height (e.g. node
        // restarted with a fresh chain or a reorg), reset to rescan from the start.
        if last_synced_height > node_synced {
            log::warn!(
                "Wallet last_synced_height ({}) exceeds chain height ({}), resetting to 0",
                last_synced_height,
                node_synced
            );
            last_synced_height = 0;
            self.store.last_synced_height = Some(0);
        }

        log::info!("Re-scanning Blockchain from:{last_synced_height} to:{node_synced}");

        let block_hash = self.rpc.get_block_hash(last_synced_height)?;
        let Header { time, .. } = self.rpc.get_block_header(&block_hash)?;

        let _ = self.import_descriptors(&descriptors_to_import, Some(time), None);

        // Returns when the scanning is completed
        loop {
            match get_wallet_scanning_details(&self.rpc)? {
                Some(ScanningDetails::Scanning { duration, .. }) => {
                    // Todo: Show scan progress
                    log::info!("Scanning for {}s", duration);
                    thread::sleep(HEART_BEAT_INTERVAL);
                    continue;
                }
                Some(ScanningDetails::NotScanning(_)) => {
                    log::info!("Scanning completed");
                    break;
                }
                None => {
                    log::info!("No scan is in progress or Scanning completed");
                    break;
                }
            }
        }
        self.finalize_sync(node_synced)
    }

    /// Electrum-style sync: register every wallet-owned script client-side,
    /// then list UTXOs via per-scripthash queries.
    fn sync_no_rescan(&mut self) -> Result<(), WalletError> {
        self.populate_backend_watched_scripts()?;
        let tip = self.rpc.get_block_count()?;
        self.finalize_sync(tip)
    }

    /// Shared tail of both sync paths: record the synced tip, refresh the
    /// UTXO cache, advance the external index, and recompute the offer-max
    /// cache.
    fn finalize_sync(&mut self, tip: u64) -> Result<(), WalletError> {
        self.store.last_synced_height = Some(tip);
        self.update_utxo_cache(self.get_all_utxo_from_rpc()?);
        let max_external_index = self.find_hd_next_index(KeychainKind::External)?;
        self.store.external_index = max_external_index;
        self.refresh_offer_maxsize_cache()?;
        Ok(())
    }

    /// Retry sync forever; handles transient RPC errors.
    fn sync_no_fail(&mut self) {
        while let Err(e) = self.sync() {
            log::error!("Blockchain sync failed. Retrying. | {e:?}");
            thread::sleep(HEART_BEAT_INTERVAL);
        }
    }

    /// Import watch addresses into core wallet. Does not check if the address was already imported.
    /// Scans blocks from a given timestamp.
    pub(crate) fn import_descriptors(
        &self,
        descriptors_to_import: &[String],
        time: Option<u32>,
        address_label: Option<String>,
    ) -> Result<(), WalletError> {
        let address_label = address_label.unwrap_or(self.get_core_wallet_label());

        // Offset by +2h because import_descriptors applies a default -2h to the timestamp
        let time_stamp = time.map(|t| json!(t + 7200)).unwrap_or(json!("now"));

        let import_requests = descriptors_to_import
            .iter()
            .map(|desc| {
                if desc.contains("/*") {
                    return json!({
                        "timestamp": time_stamp,
                        "desc": desc,
                        "range": (self.get_addrss_import_count() - 1)
                    });
                }
                json!({
                    "timestamp": time_stamp,
                    "desc": desc,
                    "label": address_label
                })
            })
            .collect();
        let _res: Vec<Value> = self.rpc.call("importdescriptors", &[import_requests])?;
        Ok(())
    }

    /// Verify the SPV proof for a transaction.
    pub fn verify_tx_out_proof(
        &self,
        expected_txid: &bitcoin::Txid,
        proof_hex: &str,
    ) -> Result<(), WalletError> {
        let proof_txids: Vec<bitcoin::Txid> = self
            .rpc
            .call("verifytxoutproof", &[json!(proof_hex)])
            .map_err(WalletError::Rpc)?;

        if proof_txids != vec![*expected_txid] {
            return Err(WalletError::MerkleProofInvalid {
                expected: *expected_txid,
                got: proof_txids,
            });
        }

        Ok(())
    }
}

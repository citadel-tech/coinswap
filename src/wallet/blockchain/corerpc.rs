//! Bitcoin Core backend: drives JSON-RPC for wallet and watchtower queries and
//! a lazily-connected ZMQ subscriber for block/tx notifications.

use std::sync::Mutex;

use bitcoin::{address::NetworkUnchecked, block::Header, Address, BlockHash, Transaction, Txid};
use bitcoind::bitcoincore_rpc::{
    json::{
        EstimateMode, EstimateSmartFeeResult, GetAddressInfoResult, GetBlockHeaderResult,
        GetBlockchainInfoResult, GetRawTransactionResult, GetTxOutResult, ListTransactionResult,
        ListUnspentResultEntry, ScanningDetails,
    },
    Auth, Client, RpcApi,
};
use serde::Deserialize;
use serde_json::Value;

use super::{BlockRef, Blockchain, WatchEvent};
use crate::wallet::error::WalletError;

/// Configuration for connecting to a Bitcoin Core node via JSON-RPC + ZMQ.
#[derive(Debug, Clone)]
pub struct CoreRpcConfig {
    /// The Bitcoin node URL.
    pub url: String,
    /// The Bitcoin node authentication mechanism.
    // TODO: Make Auth take cookies too.
    pub auth: Auth,
    /// The wallet name in the Bitcoin node.
    pub wallet_name: String,
    /// ZMQ endpoint for block/tx notifications (e.g. `"tcp://127.0.0.1:28332"`).
    /// Consumed by the watchtower notification path.
    pub zmq_addr: String,
}

const RPC_HOSTPORT: &str = "localhost:18443";

impl Default for CoreRpcConfig {
    fn default() -> Self {
        Self {
            url: RPC_HOSTPORT.to_string(),
            auth: Auth::UserPass("regtestrpcuser".to_string(), "regtestrpcpass".to_string()),
            wallet_name: "random-wallet-name".to_string(),
            zmq_addr: "tcp://127.0.0.1:28332".to_string(),
        }
    }
}

/// Lazily-connected ZMQ `rawtx`/`rawblock` subscriber for the watchtower.
///
/// The endpoint (`addr`) is always known up front; the SUB socket itself is
/// opened on demand the first time the watchtower primes or polls it. The
/// wallet's `CoreRPC` instance never does either, so it never opens a socket.
/// The socket sits behind a `Mutex` so `CoreRPC` stays `Sync` (a bare
/// `zmq::Socket` is not).
struct ZmqSubscriber {
    addr: String,
    socket: Mutex<Option<zmq::Socket>>,
}

/// Wrap a ZMQ transport problem (socket setup, connect, or subscription
/// failure) in the dedicated [`WalletError::Zmq`] kind.
fn zmq_err(msg: String) -> WalletError {
    WalletError::Zmq(msg)
}

impl ZmqSubscriber {
    fn new(addr: String) -> Self {
        Self {
            addr,
            socket: Mutex::new(None),
        }
    }

    /// Connect and subscribe to `rawtx`/`rawblock` if not already connected.
    /// Idempotent.
    ///
    /// Priming this **before** the watchtower's startup mempool scan is
    /// load-bearing: a SUB socket drops messages published during the
    /// slow-joiner handshake, so connecting first lets the subsequent mempool
    /// scan backstop the handshake window — without it, a transaction landing in
    /// that gap could be missed by both the scan and the ZMQ feed.
    fn ensure_connected(&self) -> Result<(), WalletError> {
        let mut guard = self
            .socket
            .lock()
            .map_err(|_| zmq_err("ZMQ socket mutex poisoned".to_string()))?;
        if guard.is_some() {
            return Ok(());
        }
        let ctx = zmq::Context::new();
        let socket = ctx
            .socket(zmq::SUB)
            .map_err(|e| zmq_err(format!("ZMQ socket: {e}")))?;
        socket
            .connect(&self.addr)
            .map_err(|e| zmq_err(format!("ZMQ connect {}: {e}", self.addr)))?;
        socket
            .set_subscribe(b"rawtx")
            .map_err(|e| zmq_err(format!("ZMQ subscribe rawtx: {e}")))?;
        socket
            .set_subscribe(b"rawblock")
            .map_err(|e| zmq_err(format!("ZMQ subscribe rawblock: {e}")))?;
        *guard = Some(socket);
        Ok(())
    }

    /// Read the next raw ZMQ multipart message `(topic, payload)`, non-blocking.
    /// Connects the SUB socket on first call.
    fn recv_event(&self) -> Option<(String, Vec<u8>)> {
        self.ensure_connected().ok()?;
        let guard = self.socket.lock().ok()?;
        let socket = guard.as_ref()?;
        let msg = socket.recv_multipart(zmq::DONTWAIT).ok()?;
        if msg.len() < 2 {
            return None;
        }
        Some((String::from_utf8_lossy(&msg[0]).to_string(), msg[1].clone()))
    }
}

/// Bitcoin Core backend over JSON-RPC (+ ZMQ for notifications).
///
/// Holds the RPC client, the connection config (incl. the ZMQ endpoint), and a
/// lazily-connected ZMQ subscriber — the wallet's instance never primes or polls
/// it, so it never opens a socket.
pub struct CoreRPC {
    rpc: Client,
    config: CoreRpcConfig,
    /// `rawtx`/`rawblock` subscriber, established lazily by the watchtower.
    zmq: ZmqSubscriber,
}

impl std::fmt::Debug for CoreRPC {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoreRPC")
            .field("url", &self.config.url)
            .field("wallet_name", &self.config.wallet_name)
            .finish()
    }
}

impl CoreRPC {
    /// Connect an RPC client for the configured wallet endpoint. Cheap — the
    /// client connects lazily on first use. Used by `AnyBlockchain::from_config`.
    pub fn new(config: &CoreRpcConfig) -> Result<Self, WalletError> {
        let rpc = Client::new(
            &format!("http://{}/wallet/{}", config.url, config.wallet_name),
            config.auth.clone(),
        )?;
        Ok(Self {
            rpc,
            config: config.clone(),
            zmq: ZmqSubscriber::new(config.zmq_addr.clone()),
        })
    }

    /// Open a fresh, independent Bitcoin Core RPC connection with the same config.
    ///
    /// Used to give a separate consumer (e.g. the watchtower discovery thread)
    /// its own connection without threading the config around separately.
    pub fn reconnect(&self) -> Result<Self, WalletError> {
        Self::new(&self.config)
    }

    /// Name of the Bitcoin Core wallet this backend is bound to.
    ///
    /// Only meaningful for the Core backend (Electrum has no server-side wallet),
    /// so this is an inherent `CoreRPC` method rather than part of the
    /// [`Blockchain`] trait. The wallet loader matches on
    /// `AnyBlockchain::CoreRPC` and compares this against the on-disk wallet
    /// file name.
    pub fn wallet_name(&self) -> &str {
        &self.config.wallet_name
    }

    /// Connect and subscribe the ZMQ notification socket up front.
    ///
    /// The watchtower calls this **before** its startup mempool scan so the SUB
    /// socket's slow-joiner handshake completes first (see
    /// `ZmqSubscriber::ensure_connected`). Only the Core backend has a ZMQ
    /// feed, so this is an inherent `CoreRPC` method rather than part of the
    /// [`Blockchain`] trait.
    pub fn prime_subscription(&self) -> Result<(), WalletError> {
        self.zmq.ensure_connected()
    }
}

impl Blockchain for CoreRPC {
    fn get_blockchain_info(&self) -> Result<GetBlockchainInfoResult, WalletError> {
        Ok(self.rpc.get_blockchain_info()?)
    }

    fn get_block_count(&self) -> Result<u64, WalletError> {
        Ok(self.rpc.get_block_count()?)
    }

    fn get_block_hash(&self, height: u64) -> Result<BlockHash, WalletError> {
        Ok(self.rpc.get_block_hash(height)?)
    }

    fn get_block_header(&self, hash: &BlockHash) -> Result<Header, WalletError> {
        Ok(self.rpc.get_block_header(hash)?)
    }

    fn get_block_header_info(&self, hash: &BlockHash) -> Result<GetBlockHeaderResult, WalletError> {
        Ok(self.rpc.get_block_header_info(hash)?)
    }

    fn get_raw_transaction(
        &self,
        txid: &Txid,
        block_hash: Option<&BlockHash>,
    ) -> Result<Transaction, WalletError> {
        Ok(self.rpc.get_raw_transaction(txid, block_hash)?)
    }

    fn get_raw_transaction_info(
        &self,
        txid: &Txid,
        block_hash: Option<&BlockHash>,
    ) -> Result<GetRawTransactionResult, WalletError> {
        Ok(self.rpc.get_raw_transaction_info(txid, block_hash)?)
    }

    fn get_tx_out(
        &self,
        txid: &Txid,
        vout: u32,
        include_mempool: Option<bool>,
    ) -> Result<Option<GetTxOutResult>, WalletError> {
        Ok(self.rpc.get_tx_out(txid, vout, include_mempool)?)
    }

    fn list_unspent(
        &self,
        minconf: Option<usize>,
        maxconf: Option<usize>,
    ) -> Result<Vec<ListUnspentResultEntry>, WalletError> {
        Ok(self.rpc.list_unspent(minconf, maxconf, None, None, None)?)
    }

    fn send_raw_transaction(&self, tx: &Transaction) -> Result<Txid, WalletError> {
        Ok(self.rpc.send_raw_transaction(tx)?)
    }

    fn derive_addresses(
        &self,
        descriptor: &str,
        range: Option<[u32; 2]>,
    ) -> Result<Vec<Address<NetworkUnchecked>>, WalletError> {
        Ok(self.rpc.derive_addresses(descriptor, range)?)
    }

    fn list_transactions(
        &self,
        label: Option<&str>,
        count: Option<usize>,
        skip: Option<usize>,
        include_watchonly: Option<bool>,
    ) -> Result<Vec<ListTransactionResult>, WalletError> {
        Ok(self
            .rpc
            .list_transactions(label, count, skip, include_watchonly)?)
    }

    fn estimate_smart_fee(
        &self,
        conf_target: u16,
        estimate_mode: Option<EstimateMode>,
    ) -> Result<EstimateSmartFeeResult, WalletError> {
        Ok(self.rpc.estimate_smart_fee(conf_target, estimate_mode)?)
    }

    /// Load the watch-only wallet on the node, creating it if absent. Ported
    /// verbatim from the pre-refactor `Wallet::sync` Core bootstrap so behaviour
    /// is unchanged; only invoked on the Core sync path.
    fn prepare_backend_wallet(&self, wallet_name: &str) -> Result<(), WalletError> {
        if self.rpc.list_wallets()?.contains(&wallet_name.to_string()) {
            log::debug!("wallet already loaded: {wallet_name}");
        } else if list_wallet_dir(&self.rpc)?.contains(&wallet_name.to_string()) {
            self.rpc.load_wallet(wallet_name)?;
            log::debug!("wallet loaded: {wallet_name}");
        } else {
            // pre-0.21 uses legacy wallets
            if self.rpc.version()? < 210_000 {
                self.rpc
                    .create_wallet(wallet_name, Some(true), None, None, None)?;
            } else {
                // https://github.com/rust-bitcoin/rust-bitcoincore-rpc/issues/225 is
                // still open, so we issue the call directly to request a descriptor wallet.
                let args = [
                    Value::String(wallet_name.to_string()),
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
        Ok(())
    }

    fn get_address_info(&self, addr: &Address) -> Result<GetAddressInfoResult, WalletError> {
        Ok(self.rpc.get_address_info(addr)?)
    }

    fn import_descriptors(&self, requests: &[Value]) -> Result<(), WalletError> {
        let _res: Vec<Value> = self
            .rpc
            .call("importdescriptors", &[Value::Array(requests.to_vec())])?;
        Ok(())
    }

    fn wallet_scanning_status(&self) -> Result<Option<ScanningDetails>, WalletError> {
        #[derive(Deserialize)]
        struct WalletInfoScanningOnly {
            scanning: Option<ScanningDetails>,
        }
        // Parse only the field we need so upstream schema changes (e.g. v30
        // balance-field removals) do not break deserialization.
        let info: WalletInfoScanningOnly = self.rpc.call("getwalletinfo", &[])?;
        Ok(info.scanning)
    }

    fn poll_event(&self) -> Option<WatchEvent> {
        let (topic, payload) = self.zmq.recv_event()?;
        match topic.as_str() {
            "rawtx" => Some(WatchEvent::TxSeen { raw_tx: payload }),
            "rawblock" => Some(WatchEvent::BlockConnected(BlockRef {
                height: 0,
                hash: payload,
            })),
            _ => None,
        }
    }

    fn get_raw_mempool(&self) -> Result<Vec<Txid>, WalletError> {
        Ok(self.rpc.get_raw_mempool()?)
    }

    fn chain_name(&self) -> Result<String, WalletError> {
        Ok(self.rpc.get_blockchain_info()?.chain.to_string())
    }
}

/// List the wallet names known to the node (`listwalletdir`).
fn list_wallet_dir(client: &Client) -> Result<Vec<String>, WalletError> {
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

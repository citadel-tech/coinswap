//! Download, process and store Maker offers from the directory-server.
//!
//! It defines structures like [`OfferAndAddress`] and [`MakerAddress`] for representing maker offers and addresses.
//! The [`OfferBook`] struct keeps track of good and bad makers, and it provides methods for managing offers.
//! The module handles the syncing of the offer book with addresses obtained from directory servers and local configurations.
//! It uses asynchronous channels for concurrent processing of maker offers.

use std::{
    collections::HashSet,
    convert::TryFrom,
    fmt,
    io::BufWriter,
    net::TcpStream,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, RwLock,
    },
    thread::{self, sleep, Builder, JoinHandle},
    time::Duration,
};

use bitcoin::hashes::Hash;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use socks::Socks5Stream;

use crate::{
    protocol::{
        error::ProtocolError,
        messages::{FidelityProof, GiveOffer, MakerToTakerMessage, Offer, TakerToMakerMessage},
        messages2::{self, GetOffer},
    },
    taker::{
        api::{
            FIRST_CONNECT_ATTEMPTS, FIRST_CONNECT_ATTEMPT_TIMEOUT_SEC,
            FIRST_CONNECT_SLEEP_DELAY_SEC,
        },
        api2::connect_to_maker,
        routines::handshake_maker,
    },
    utill::{read_message, send_message, verify_fidelity_checks},
    watch_tower::{rpc_backend::BitcoinRpc, service::WatchService, watcher::WatcherEvent},
};

use super::error::TakerError;

#[cfg(not(feature = "integration-test"))]
const OFFER_SYNC_INTERVAL: Duration = Duration::from_secs(10 * 60);

#[cfg(feature = "integration-test")]
const OFFER_SYNC_INTERVAL: Duration = Duration::from_secs(2);

/// Represents an offer along with the corresponding maker address.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OfferAndAddress {
    /// Details for Maker Offer
    pub offer: Offer,
    /// Maker Address: onion_addr:port
    pub address: MakerAddress,
    /// When this offer had been downloaded and cached locally
    pub timestamp: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct OnionAddress {
    port: String,
    onion_addr: String,
}

/// Enum representing maker addresses.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Hash)]
pub struct MakerAddress(OnionAddress);

impl fmt::Display for MakerAddress {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}:{}", self.0.onion_addr, self.0.port)
    }
}

impl TryFrom<&mut TcpStream> for MakerAddress {
    type Error = std::io::Error;
    fn try_from(value: &mut TcpStream) -> Result<Self, Self::Error> {
        let socket_addr = value.peer_addr()?;
        Ok(MakerAddress(OnionAddress {
            port: socket_addr.port().to_string(),
            onion_addr: socket_addr.ip().to_string(),
        }))
    }
}

/// OfferBookHandle, api interface to interact with
/// offerbook
#[derive(Clone)]
pub struct OfferBookHandle {
    inner: Arc<RwLock<OfferBook>>,
    path: PathBuf,
}

impl OfferBookHandle {
    /// Gets the current snapshot of whole offerbook
    pub fn snapshot(&self) -> OfferBook {
        self.inner.read().unwrap().clone()
    }

    /// Tag a maker as bad
    pub fn add_bad_maker(&self, maker: &OfferAndAddress) {
        self.inner.write().unwrap().add_bad_maker(maker);
    }

    /// Add a new offer from maker
    pub fn add_new_offer(&self, offer: OfferAndAddress) {
        self.inner.write().unwrap().add_new_offer(&offer);
    }

    /// All current good makers
    pub fn all_good_makers(&self) -> Vec<OfferAndAddress> {
        #[cfg(not(feature = "integration-test"))]
        {
            self.inner.read().unwrap().all_good_makers()
        }
        #[cfg(feature = "integration-test")]
        {
            use std::{
                thread::sleep,
                time::{Duration, Instant},
            };

            const POLL_INTERVAL_MS: u64 = 200;
            const MAX_WAIT_SECS: u64 = 30;

            let start = Instant::now();

            loop {
                let snapshot = self.inner.read().unwrap().all_good_makers();

                if !snapshot.is_empty() {
                    return snapshot;
                }

                if start.elapsed().as_secs() >= MAX_WAIT_SECS {
                    return snapshot;
                }

                sleep(Duration::from_millis(POLL_INTERVAL_MS));
            }
        }
    }

    /// All bad makers
    pub fn get_bad_makers(&self) -> Vec<OfferAndAddress> {
        self.inner.read().unwrap().get_bad_makers()
    }

    /// Fetches current free addresses
    pub fn fresh_addresses(&self) -> HashSet<MakerAddress> {
        self.inner
            .read()
            .unwrap()
            .get_fresh_addrs()
            .into_iter()
            .map(|oa| oa.address)
            .collect()
    }

    /// Checks if an address is bad or not
    pub fn is_bad_maker(&self, offer_and_address: &OfferAndAddress) -> bool {
        self.inner
            .read()
            .unwrap()
            .bad_makers
            .contains(offer_and_address)
    }

    /// Persist offerbook on disk
    pub fn persist(&self) -> Result<(), TakerError> {
        self.inner.read().unwrap().write_to_disk(&self.path)
    }

    /// Create or load offerbook on disk
    pub fn load_or_create(data_dir: &Path) -> Result<Self, TakerError> {
        let path = data_dir.join("offerbook.json");

        let offerbook = if path.exists() {
            match OfferBook::read_from_disk(&path) {
                Ok(book) => {
                    log::info!("Successfully loaded offerbook at {path:?}");
                    book
                }
                Err(e) => {
                    log::error!("Offerbook corrupted at {path:?}. Recreating. Error: {e:?}");
                    let book = OfferBook::default();
                    book.write_to_disk(&path)?;
                    book
                }
            }
        } else {
            log::info!("Offerbook not found. Creating new at {path:?}");
            let empty_book = OfferBook::default();
            let file = std::fs::File::create(&path)?;
            let writer = BufWriter::new(file);
            serde_json::to_writer_pretty(writer, &empty_book)?;
            empty_book
        };

        Ok(Self {
            inner: Arc::new(RwLock::new(offerbook)),
            path,
        })
    }
}

/// Service run on taker to check if the offerbook makers are active or not
pub struct OfferSyncService {
    offerbook: OfferBookHandle,
    watch_service: WatchService,
    socks_port: u16,
    rpc_backend: BitcoinRpc,
}

/// OfferSync handle, use for shutting down OfferSyncService
pub struct OfferSyncHandle {
    shutdown: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl OfferSyncHandle {
    /// Shutdown handler
    pub fn shutdown(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);

        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl OfferSyncService {
    /// Constructor method
    pub fn new(
        offerbook: OfferBookHandle,
        watch_service: WatchService,
        socks_port: u16,
        rpc_backend: BitcoinRpc,
    ) -> Self {
        Self {
            offerbook,
            watch_service,
            socks_port,
            rpc_backend,
        }
    }

    fn run_once_with<F>(&self, fetch_offers: F) -> Result<(), TakerError>
    where
        F: FnOnce(Vec<MakerAddress>, u16) -> Result<Vec<OfferAndAddress>, TakerError>,
    {
        
        let Some(WatcherEvent::MakerAddresses { maker_addresses }) =
            self.watch_service.request_maker_address()
        else {
            return Ok(());
        };

        let maker_addresses = maker_addresses
            .into_iter()
            .map(MakerAddress::try_from)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        let fresh = self.offerbook.fresh_addresses();

        let to_fetch = maker_addresses
            .into_iter()
            .filter(|a| !fresh.contains(a))
            .collect::<Vec<_>>();

        let offers = fetch_offers(to_fetch, self.socks_port)?;

        for new_offer in offers {
            if let Err(e) = self
                .verify_fidelity_proof(&new_offer.offer.fidelity, &new_offer.address.to_string())
            {
                log::warn!(
                    "Fidelity Proof Verification failed with error: {:?}. Adding this to bad maker list: {}",
                    e,
                    new_offer.address
                );
                self.offerbook.add_bad_maker(&new_offer);
            } else {
                log::info!("Fidelity Bond verification success. Adding offer to our OfferBook");
                self.offerbook.add_new_offer(new_offer);
            }
        }

        self.offerbook.persist()?;
        Ok(())
    }

    fn run_legacy_once(&self) -> Result<(), TakerError> {
        self.run_once_with(fetch_offer_from_makers)
    }

    fn run_taproot_once(&self) -> Result<(), TakerError> {
        self.run_once_with(fetch_taproot_offers)
    }

    fn start_with<F>(self, run_once: F) -> OfferSyncHandle
    where
        F: Fn(&OfferSyncService) -> Result<(), TakerError> + Send + 'static,
    {
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_thread = shutdown.clone();

        let join = std::thread::Builder::new()
            .name("offer-sync-service".to_string())
            .spawn(move || {
                log::info!("Offer sync service started");

                while !shutdown_thread.load(Ordering::Relaxed) {
                    if let Err(e) = run_once(&self) {
                        log::warn!("Offer sync iteration failed: {e:?}");
                    }

                    let mut slept = Duration::ZERO;
                    while slept < OFFER_SYNC_INTERVAL && !shutdown_thread.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_secs(1));
                        slept += Duration::from_secs(1);
                    }
                }

                log::info!("Offer sync service stopped");
            })
            .expect("failed to spawn offer sync service");

        OfferSyncHandle {
            shutdown,
            join: Some(join),
        }
    }

    /// Start offerbook fetch background service for legacy
    pub fn start_legacy(self) -> OfferSyncHandle {
        self.start_with(|svc| svc.run_legacy_once())
    }

    /// Start offerbook fetch background service for taproot.
    pub fn start_taproot(self) -> OfferSyncHandle {
        self.start_with(|svc| svc.run_taproot_once())
    }

    /// do the fidelity proof
    fn verify_fidelity_proof(
        &self,
        proof: &FidelityProof,
        onion_addr: &str,
    ) -> Result<(), TakerError> {
        let txid = proof.bond.outpoint.txid;
        let transaction = self.rpc_backend.get_raw_tx(&txid)?;
        let current_height = self.rpc_backend.get_block_count()?;

        verify_fidelity_checks(proof, onion_addr, transaction, current_height)
            .map_err(TakerError::Wallet)
    }
}

/// An ephemeral Offerbook tracking good and bad makers. Currently, Offerbook is initiated
/// at the start of every swap. So good and bad maker list will not be persisted.
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct OfferBook {
    pub(super) all_makers: Vec<OfferAndAddress>,
    pub(super) bad_makers: Vec<OfferAndAddress>,
}

impl OfferBook {
    /// Gets all "not-bad" offers.
    fn all_good_makers(&self) -> Vec<OfferAndAddress> {
        self.all_makers
            .iter()
            .filter(|offer| !self.bad_makers.contains(offer))
            .cloned()
            .collect()
    }
    /// Gets all offers.
    pub fn all_makers(&self) -> Vec<&OfferAndAddress> {
        self.all_makers.iter().collect()
    }

    /// Adds a new offer to the offer book.
    fn add_new_offer(&mut self, offer: &OfferAndAddress) -> bool {
        let timestamped = OfferAndAddress {
            offer: offer.offer.clone(),
            address: offer.address.clone(),
            timestamp: Utc::now(),
        };
        if !self.all_makers.iter().any(|to| to.offer == offer.offer) {
            self.all_makers.push(timestamped);
            true
        } else {
            false
        }
    }

    /// Gets the list of addresses which were last update less than 30 minutes ago.
    /// We exclude these addresses from the next round of downloading offers.
    fn get_fresh_addrs(&self) -> Vec<OfferAndAddress> {
        let now = Utc::now();
        self.all_makers
            .iter()
            .filter(|to| (now - to.timestamp).num_minutes() < 30)
            .cloned()
            .collect()
    }

    /// Adds a bad maker to the offer book.
    fn add_bad_maker(&mut self, bad_maker: &OfferAndAddress) -> bool {
        if !self.bad_makers.contains(bad_maker) {
            self.bad_makers.push(bad_maker.clone());
            true
        } else {
            false
        }
    }

    /// Gets the list of bad makers.
    fn get_bad_makers(&self) -> Vec<OfferAndAddress> {
        self.bad_makers.to_vec()
    }

    /// Load existing file, updates it, writes it back (errors if path doesn't exist).
    fn write_to_disk(&self, path: &Path) -> Result<(), TakerError> {
        let offerdata_file = std::fs::OpenOptions::new().write(true).open(path)?;
        let writer = BufWriter::new(offerdata_file);
        Ok(serde_json::to_writer_pretty(writer, &self)?)
    }

    /// Reads from a path (errors if path doesn't exist).
    fn read_from_disk(path: &Path) -> Result<Self, TakerError> {
        let content = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&content)?)
    }
}

/// Synchronizes the offer book with specific maker addresses.
pub(crate) fn fetch_offer_from_makers(
    maker_addresses: Vec<MakerAddress>,
    socks_port: u16,
) -> Result<Vec<OfferAndAddress>, TakerError> {
    let (offers_writer, offers_reader) = mpsc::channel::<Option<OfferAndAddress>>();
    // Thread pool for all connections to fetch maker offers.
    let mut thread_pool = Vec::new();
    let maker_addresses_len = maker_addresses.len();
    for addr in maker_addresses {
        let offers_writer = offers_writer.clone();
        let thread = Builder::new()
            .name(format!("maker_offer_fetch_thread_{addr}"))
            .spawn(move || -> Result<(), TakerError> {
                let offer = addr.download_maker_offer(socks_port);
                Ok(offers_writer.send(offer)?)
            })?;

        thread_pool.push(thread);
    }
    let mut result = Vec::new();
    for _ in 0..maker_addresses_len {
        if let Some(offer_addr) = offers_reader.recv()? {
            result.push(offer_addr);
        }
    }

    for thread in thread_pool {
        let join_result = thread.join();

        if let Err(e) = join_result {
            log::error!("Error while joining thread: {e:?}");
        }
    }
    Ok(result)
}

/// Fetch offers from taproot makers using taproot protocol messages
fn fetch_taproot_offers(
    maker_addresses: Vec<MakerAddress>,
    socks_port: u16,
) -> Result<Vec<OfferAndAddress>, TakerError> {
    use std::sync::mpsc;

    let (offers_writer, offers_reader) = mpsc::channel::<Option<OfferAndAddress>>();
    let maker_addresses_len = maker_addresses.len();

    thread::scope(|s| {
        for address in maker_addresses {
            let offers_writer = offers_writer.clone();
            s.spawn(move || {
                let offer = address.download_taproot_offer(socks_port);
                let _ = offers_writer.send(offer);
            });
        }
    });

    let mut result = Vec::new();
    for _ in 0..maker_addresses_len {
        if let Some(offer_addr) = offers_reader.recv()? {
            result.push(offer_addr);
        }
    }

    Ok(result)
}

impl TryFrom<String> for OnionAddress {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let mut parts = value.splitn(2, ':');
        let onion_addr = parts.next().ok_or("Missing onion address")?.to_string();
        let port = parts.next().ok_or("Missing port")?.to_string();

        if onion_addr.is_empty() || port.is_empty() {
            return Err("Empty onion address or port");
        }

        Ok(OnionAddress { onion_addr, port })
    }
}

impl TryFrom<String> for MakerAddress {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let onion = OnionAddress::try_from(value)?;
        Ok(MakerAddress(onion))
    }
}

impl MakerAddress {
    fn download_maker_offer_attempt_once(&self, socks_port: u16) -> Result<Offer, TakerError> {
        let maker_addr = self.to_string();
        log::info!("Downloading offer from {maker_addr}");
        let mut socket = if cfg!(feature = "integration-test") {
            TcpStream::connect(&maker_addr)?
        } else {
            Socks5Stream::connect(
                format!("127.0.0.1:{socks_port}").as_str(),
                maker_addr.as_ref(),
            )?
            .into_inner()
        };

        socket.set_read_timeout(Some(Duration::from_secs(FIRST_CONNECT_ATTEMPT_TIMEOUT_SEC)))?;
        socket.set_write_timeout(Some(Duration::from_secs(FIRST_CONNECT_ATTEMPT_TIMEOUT_SEC)))?;

        handshake_maker(&mut socket)?;

        send_message(&mut socket, &TakerToMakerMessage::ReqGiveOffer(GiveOffer))?;

        let msg_bytes = read_message(&mut socket)?;
        let msg: MakerToTakerMessage = serde_cbor::from_slice(&msg_bytes)?;
        let offer = match msg {
            MakerToTakerMessage::RespOffer(offer) => offer,
            msg => {
                return Err(ProtocolError::WrongMessage {
                    expected: "RespOffer".to_string(),
                    received: format!("{msg}"),
                }
                .into());
            }
        };

        log::info!("Downloaded offer from : {maker_addr} ");

        Ok(*offer)
    }

    fn download_maker_offer(self, socks_port: u16) -> Option<OfferAndAddress> {
        let mut ii = 0;

        loop {
            ii += 1;
            match self.download_maker_offer_attempt_once(socks_port) {
                Ok(offer) => {
                    return Some(OfferAndAddress {
                        offer,
                        address: self,
                        timestamp: Utc::now(),
                    })
                }
                Err(e) => {
                    if ii <= FIRST_CONNECT_ATTEMPTS {
                        log::warn!(
                            "Failed to request offer from maker {self}, with error: {e:?} reattempting {ii} of {FIRST_CONNECT_ATTEMPTS}"
                        );
                        sleep(Duration::from_millis(FIRST_CONNECT_SLEEP_DELAY_SEC));
                        continue;
                    } else {
                        log::error!(
                            "Connection attempt exceeded for request offer from maker {self}"
                        );
                        return None;
                    }
                }
            }
        }
    }

    /// Download a single offer from a taproot maker
    fn download_taproot_offer(self, socks_port: u16) -> Option<OfferAndAddress> {
        let maker_addr = self.to_string();
        log::info!("Downloading offer from taproot maker: {}", maker_addr);

        // Try connecting to the maker using the helper function
        let mut socket = match connect_to_maker(&maker_addr, socks_port, 30) {
            Ok(s) => s,
            Err(e) => {
                log::error!("Failed to connect to taproot maker {}: {:?}", maker_addr, e);
                return None;
            }
        };

        // Send GetOffer message (taproot protocol)
        let get_offer_msg = GetOffer {
            id: String::new(),
            protocol_version_min: 1,
            protocol_version_max: 1,
            number_of_transactions: 1,
        };

        let msg = messages2::TakerToMakerMessage::GetOffer(get_offer_msg);
        if let Err(e) = send_message(&mut socket, &msg) {
            log::error!("Failed to send GetOffer message to {}: {:?}", maker_addr, e);
            return None;
        }

        // Read the response
        let response_bytes = match read_message(&mut socket) {
            Ok(bytes) => bytes,
            Err(e) => {
                log::error!("Failed to read response from {}: {:?}", maker_addr, e);
                return None;
            }
        };

        // Parse the response
        let response: messages2::MakerToTakerMessage = match serde_cbor::from_slice(&response_bytes)
        {
            Ok(msg) => msg,
            Err(e) => {
                log::error!("Failed to parse response from {}: {:?}", maker_addr, e);
                return None;
            }
        };

        // Extract the offer
        let taproot_offer = match response {
            messages2::MakerToTakerMessage::RespOffer(offer) => *offer,
            _ => {
                log::error!(
                    "Unexpected response from {}: expected RespOffer",
                    maker_addr
                );
                return None;
            }
        };

        let offer = crate::protocol::messages::Offer {
            base_fee: taproot_offer.base_fee,
            amount_relative_fee_pct: taproot_offer.amount_relative_fee,
            time_relative_fee_pct: taproot_offer.time_relative_fee,
            required_confirms: 1, // Default value for taproot
            minimum_locktime: taproot_offer.minimum_locktime,
            max_size: taproot_offer.max_size,
            min_size: taproot_offer.min_size,
            tweakable_point: taproot_offer.tweakable_point,
            fidelity: crate::protocol::messages::FidelityProof {
                bond: taproot_offer.fidelity.bond,
                cert_hash: bitcoin::hashes::sha256d::Hash::from_slice(
                    taproot_offer.fidelity.cert_hash.as_ref(),
                )
                .ok()?,
                cert_sig: taproot_offer.fidelity.cert_sig,
            },
        };

        log::info!(
            "Successfully downloaded offer from taproot maker: {}",
            maker_addr
        );

        Some(OfferAndAddress {
            offer,
            address: self,
            timestamp: Utc::now(),
        })
    }
}

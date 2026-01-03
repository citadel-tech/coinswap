//! Download, process and store Maker offers from the directory-server.
//!
//! It defines structures like [`OfferAndAddress`] and [`MakerAddress`] for representing maker offers and addresses.
//! The [`OfferBook`] struct keeps track of good and bad makers, and it provides methods for managing offers.
//! The module handles the syncing of the offer book with addresses obtained from directory servers and local configurations.
//! It uses asynchronous channels for concurrent processing of maker offers.

use std::{
    convert::TryFrom,
    fmt,
    io::BufWriter,
    net::TcpStream,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, RwLock,
    },
    thread::{sleep, Builder, JoinHandle},
    time::Duration,
};

use bitcoin::hashes::Hash;
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
const OFFER_SYNC_INTERVAL: Duration = Duration::from_secs(15 * 60);

#[cfg(feature = "integration-test")]
const OFFER_SYNC_INTERVAL: Duration = Duration::from_secs(10);

// ##TODO: Split these constructs and API's well, in a separate module.

/// Represents an offer along with the corresponding maker address.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OfferAndAddress {
    /// Details for Maker Offer
    pub offer: Offer,
    /// Maker Address: onion_addr:port
    pub address: MakerAddress,
    /// Current state of maker
    pub state: MakerState,
    /// Supporting protocol (Legacy or Taproot)
    pub protocol: MakerProtocol,
}

/// Canonical maker record.
/// A maker may or may not currently have an offer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MakerOfferCandidate {
    /// Maker Address: onion_addr:port
    pub address: MakerAddress,

    /// Latest offer, if successfully fetched
    pub offer: Option<Offer>,

    /// Current state of maker
    pub state: MakerState,

    /// Supporting protocol (Legacy or Taproot), if known
    pub protocol: Option<MakerProtocol>,
}

impl MakerOfferCandidate {
    fn mark_success(&mut self, offer: Offer, protocol: MakerProtocol) {
        self.offer = Some(offer);
        self.protocol = Some(protocol);
        if self.state != MakerState::Bad {
            self.state = MakerState::Good;
        }
    }

    fn mark_failure(&mut self) {
        self.state = match self.state {
            MakerState::Good => MakerState::Unresponsive { retries: 1 },
            MakerState::Unresponsive { retries } if retries < 10 => MakerState::Unresponsive {
                retries: retries + 1,
            },
            MakerState::Unresponsive { .. } => MakerState::Bad,
            MakerState::Bad => MakerState::Bad,
        };
    }

    fn as_offer_and_address(&self) -> Option<OfferAndAddress> {
        match (&self.offer, &self.protocol) {
            (Some(offer), Some(protocol)) => Some(OfferAndAddress {
                offer: offer.clone(),
                address: self.address.clone(),
                state: self.state.clone(),
                protocol: protocol.clone(),
            }),
            _ => None,
        }
    }
}

/// Represents the Maker connection state
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MakerState {
    /// Maker is responding to offer calls.
    Good,
    /// Maker is not responding to offer calls.
    Unresponsive {
        /// We allow only 10 retries before marking
        /// a maker as bad.
        retries: u8,
    },
    /// Maker either explicitly or because not responding
    /// is marked bad.
    Bad,
}

/// Protocol which maker follows
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MakerProtocol {
    /// Legacy
    Legacy,
    /// Taproot
    Taproot,
}

impl fmt::Display for MakerProtocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MakerProtocol::Legacy => f.write_str("Legacy"),
            MakerProtocol::Taproot => f.write_str("Taproot"),
        }
    }
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
        self.inner.write().unwrap().mark_bad(&maker.address);
    }

    /// All current good makers
    pub fn active_makers(&self, protocol: &MakerProtocol) -> Vec<OfferAndAddress> {
        #[cfg(not(feature = "integration-test"))]
        {
            self.inner.read().unwrap().active_makers(protocol)
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
                let snapshot = self.inner.read().unwrap().active_makers(protocol);

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

    /// Fetch all good makers
    pub fn good_makers(&self) -> Vec<OfferAndAddress> {
        self.inner.read().unwrap().good_makers()
    }

    /// All bad makers
    pub fn get_bad_makers(&self, protocol: &MakerProtocol) -> Vec<OfferAndAddress> {
        self.inner.read().unwrap().get_bad_makers(protocol)
    }

    /// Fetch all makers good, bad, and unresponsive
    pub fn all_makers(&self) -> Vec<MakerOfferCandidate> {
        self.inner.read().unwrap().all_makers()
    }

    /// Checks if an address is bad or not
    pub fn is_bad_maker(&self, offer_and_address: &OfferAndAddress) -> bool {
        let offerbook = self.inner.write().unwrap();
        let value = offerbook
            .makers
            .iter()
            .find(|offer| offer.address == offer_and_address.address);

        if let Some(offer) = value {
            return offer.state == MakerState::Bad;
        }
        true
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
    is_syncing: Arc<AtomicBool>,
    run_now: Arc<AtomicBool>,
}

/// OfferSync handle, use for shutting down OfferSyncService
pub struct OfferSyncHandle {
    shutdown: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    is_syncing: Arc<AtomicBool>,
    run_now: Arc<AtomicBool>,
}

impl OfferSyncHandle {
    /// Shutdown handler
    pub fn shutdown(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);

        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }

    /// Suggests whether offerbook syncing is in progress or not.
    pub fn is_syncing(&self) -> bool {
        self.is_syncing.load(Ordering::SeqCst)
    }

    /// Runs manual sync, rather than waiting for routine to trigger it.
    pub fn run_sync_now(&self) {
        self.run_now.store(true, Ordering::Relaxed);
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
            is_syncing: Arc::new(AtomicBool::new(false)),
            run_now: Arc::new(AtomicBool::new(false)),
        }
    }

    fn run_once(&self) -> Result<(), TakerError> {
        if let Some(WatcherEvent::MakerAddresses { maker_addresses }) =
            self.watch_service.request_maker_address()
        {
            let mut book = self.offerbook.inner.write().unwrap();
            for addr in maker_addresses {
                book.upsert_address(MakerAddress::try_from(addr).unwrap());
            }
        }

        let to_poll = self.offerbook.inner.read().unwrap().makers_to_poll();

        if to_poll.is_empty() {
            return Ok(());
        }

        let offers = fetch_offer_from_makers(to_poll.clone(), self.socks_port)?;

        {
            let mut book = self.offerbook.inner.write().unwrap();
            for addr in &to_poll {
                book.mark_failure(addr);
            }
        }

        for oa in offers {
            match self.verify_fidelity_proof(&oa.offer.fidelity, &oa.address.to_string()) {
                Ok(_) => {
                    self.offerbook.inner.write().unwrap().mark_success(
                        &oa.address,
                        oa.offer,
                        oa.protocol,
                    );
                }
                Err(_) => {
                    self.offerbook
                        .inner
                        .write()
                        .unwrap()
                        .mark_failure(&oa.address);
                }
            }
        }

        self.offerbook.persist()?;
        Ok(())
    }

    /// Starts the offerbook service
    pub fn start(self) -> OfferSyncHandle {
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_flag = shutdown.clone();
        let is_syncing = self.is_syncing.clone();
        let run_now = self.run_now.clone();
        self.is_syncing.store(true, Ordering::SeqCst);

        let join = std::thread::Builder::new()
            .name("offer-sync-service".to_string())
            .spawn(move || {
                log::info!("Offer sync service started");
                #[cfg(feature = "integration-test")]
                std::thread::sleep(Duration::from_secs(7));
                while !shutdown_flag.load(Ordering::Relaxed) {
                    self.is_syncing.store(true, Ordering::SeqCst);
                    log::info!("Running offerbook sync");
                    if let Err(e) = self.run_once() {
                        log::warn!("Offer sync iteration failed: {e:?}");
                    }
                    log::debug!("Running offerbook sync completed");
                    self.is_syncing.store(false, Ordering::SeqCst);
                    let mut slept = Duration::ZERO;
                    while slept < OFFER_SYNC_INTERVAL
                        && !self.run_now.load(Ordering::Relaxed)
                        && !shutdown_flag.load(Ordering::Relaxed)
                    {
                        std::thread::sleep(Duration::from_secs(1));
                        slept += Duration::from_secs(1);
                    }
                    if self.run_now.swap(false, Ordering::Relaxed) {
                        log::info!("Manual offerbook syncing initiated");
                    }
                }

                log::debug!("Offer sync service stopped");
            })
            .expect("failed to spawn offer sync service");

        OfferSyncHandle {
            shutdown,
            join: Some(join),
            is_syncing,
            run_now,
        }
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
    pub(super) makers: Vec<MakerOfferCandidate>,
}

impl OfferBook {
    fn upsert_address(&mut self, address: MakerAddress) {
        if self.makers.iter().any(|m| m.address == address) {
            return;
        }

        self.makers.push(MakerOfferCandidate {
            address,
            offer: None,
            state: MakerState::Unresponsive { retries: 0 },
            protocol: None,
        });
    }

    fn mark_success(&mut self, address: &MakerAddress, offer: Offer, protocol: MakerProtocol) {
        if let Some(m) = self.makers.iter_mut().find(|m| &m.address == address) {
            m.mark_success(offer, protocol);
        }
    }

    fn mark_failure(&mut self, address: &MakerAddress) {
        if let Some(m) = self.makers.iter_mut().find(|m| &m.address == address) {
            m.mark_failure();
        }
    }

    fn makers_to_poll(&self) -> Vec<MakerAddress> {
        self.makers
            .iter()
            .filter(|m| !matches!(m.state, MakerState::Bad))
            .map(|m| m.address.clone())
            .collect()
    }

    fn mark_bad(&mut self, address: &MakerAddress) {
        if let Some(m) = self.makers.iter_mut().find(|m| &m.address == address) {
            m.state = MakerState::Bad;
        }
    }

    /// Gets all active (good) offers for a given protocol.
    fn active_makers(&self, protocol: &MakerProtocol) -> Vec<OfferAndAddress> {
        let mut makers = self.makers.clone();
        makers.sort_by(|a, b| a.address.0.port.cmp(&b.address.0.port));
        makers
            .iter()
            .filter(|m| m.state == MakerState::Good)
            .filter(|m| m.protocol.as_ref() == Some(protocol))
            .filter_map(|m| m.as_offer_and_address())
            .collect()
    }

    fn good_makers(&self) -> Vec<OfferAndAddress> {
        self.makers
            .iter()
            .filter(|m| !matches!(m.state, MakerState::Bad))
            .filter_map(|m| m.as_offer_and_address())
            .collect()
    }

    /// Gets all offers.
    pub fn all_makers(&self) -> Vec<MakerOfferCandidate> {
        self.makers.to_vec()
    }

    /// Gets the list of bad makers.
    fn get_bad_makers(&self, protocol: &MakerProtocol) -> Vec<OfferAndAddress> {
        let mut makers = self.makers.clone();
        makers.sort_by(|a, b| a.address.0.port.len().cmp(&b.address.0.port.len()));
        makers
            .iter()
            .filter(|m| m.state == MakerState::Bad)
            .filter(|m| m.protocol.as_ref() == Some(protocol))
            .filter_map(|m| m.as_offer_and_address())
            .collect()
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

// ##TODO: Don't spawn thread for each IO request, make this better.
/// Synchronizes the offer book with specific maker addresses.
/// Tries legacy first, then taproot.
pub(crate) fn fetch_offer_from_makers(
    maker_addresses: Vec<MakerAddress>,
    socks_port: u16,
) -> Result<Vec<OfferAndAddress>, TakerError> {
    let (tx, rx) = mpsc::channel::<Option<OfferAndAddress>>();
    let total = maker_addresses.len();
    let mut threads = Vec::with_capacity(total);

    for addr in maker_addresses {
        let tx = tx.clone();
        let thread = Builder::new()
            .name(format!("maker_offer_fetch_thread_{addr}"))
            .spawn(move || {
                let offer = addr.download_offer_with_retries(socks_port);
                let _ = tx.send(offer);
            })?;

        threads.push(thread);
    }

    let mut offers = Vec::new();
    for _ in 0..total {
        if let Some(offer) = rx.recv()? {
            offers.push(offer);
        }
    }

    for t in threads {
        if let Err(e) = t.join() {
            log::error!("Error joining maker offer thread: {e:?}");
        }
    }

    Ok(offers)
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
    fn download_offer_with_retries(self, socks_port: u16) -> Option<OfferAndAddress> {
        for attempt in 1..=FIRST_CONNECT_ATTEMPTS {
            match self.clone().download_offer_auto(socks_port) {
                Ok(offer) => return Some(offer),
                Err(e) if attempt < FIRST_CONNECT_ATTEMPTS => {
                    log::debug!(
                        "Failed to fetch offer from {} (attempt {}/{}): {:?}",
                        self,
                        attempt,
                        FIRST_CONNECT_ATTEMPTS,
                        e
                    );
                    sleep(Duration::from_millis(FIRST_CONNECT_SLEEP_DELAY_SEC));
                }
                Err(e) => {
                    log::debug!("Exhausted retries fetching offer from {}: {:?}", self, e);
                }
            }
        }
        None
    }

    fn download_offer_auto(self, socks_port: u16) -> Result<OfferAndAddress, TakerError> {
        match self.fetch_legacy_offer(socks_port) {
            Ok(offer) => {
                return Ok(OfferAndAddress {
                    offer,
                    address: self,
                    state: MakerState::Good,
                    protocol: MakerProtocol::Legacy,
                });
            }
            Err(e) => {
                log::debug!("Legacy offer fetch failed for {}: {:?}", self, e);
            }
        }

        match self.clone().fetch_taproot_offer(socks_port) {
            Ok(offer) => {
                return Ok(OfferAndAddress {
                    offer,
                    address: self,
                    state: MakerState::Good,
                    protocol: MakerProtocol::Taproot,
                });
            }
            Err(e) => {
                log::debug!("Taproot offer fetch failed for {}: {:?}", self, e);
            }
        }

        Err(TakerError::General(
            "maker does not support legacy or taproot offer exchange".into(),
        ))
    }

    fn fetch_legacy_offer(&self, socks_port: u16) -> Result<Offer, TakerError> {
        let maker_addr = self.to_string();
        log::debug!("Downloading offer from {maker_addr}");
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

        log::info!("Successfully Downloaded offer from : {maker_addr} ");

        Ok(*offer)
    }

    /// Download a single offer from a taproot maker
    fn fetch_taproot_offer(self, socks_port: u16) -> Result<Offer, TakerError> {
        let maker_addr = self.to_string();
        log::debug!("Downloading offer from taproot maker: {}", maker_addr);
        let mut socket = connect_to_maker(&maker_addr, socks_port, 30)?;

        let get_offer_msg = GetOffer {
            id: String::new(),
            protocol_version_min: 1,
            protocol_version_max: 1,
            number_of_transactions: 1,
        };

        let msg = messages2::TakerToMakerMessage::GetOffer(get_offer_msg);
        send_message(&mut socket, &msg)?;

        let response_bytes = read_message(&mut socket)?;

        let response: messages2::MakerToTakerMessage = serde_cbor::from_slice(&response_bytes)?;

        let taproot_offer = match response {
            messages2::MakerToTakerMessage::RespOffer(offer) => *offer,
            msg => {
                return Err(ProtocolError::WrongMessage {
                    expected: "RespOffer".to_string(),
                    received: format!("{msg}"),
                }
                .into());
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
                .unwrap(),
                cert_sig: taproot_offer.fidelity.cert_sig,
            },
        };

        log::info!(
            "Successfully downloaded offer from taproot maker: {}",
            maker_addr
        );

        Ok(offer)
    }
}

/// Format state
pub fn format_state(state: &MakerState) -> String {
    match state {
        MakerState::Good => "Good".into(),
        MakerState::Unresponsive { retries } => {
            format!("Unresponsive (retries: {retries})")
        }
        MakerState::Bad => "Bad".into(),
    }
}

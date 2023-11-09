//TODO the wallet file format is probably best handled with sqlite

use std::{collections::HashMap, convert::TryFrom, io::Read, path::PathBuf};

use bip39::Mnemonic;
use bitcoin::{bip32::ExtendedPrivKey, Network, OutPoint, ScriptBuf};
use std::fs::{File, OpenOptions};

use super::{error::WalletError, SwapCoin};

use super::swapcoin::{IncomingSwapCoin, OutgoingSwapCoin};

use super::fidelity::generate_fidelity_scripts;

const WALLET_FILE_VERSION: u32 = 0;

#[derive(serde::Serialize, serde::Deserialize)]
struct FileData {
    wallet_name: String,
    version: u32,
    network: Network,
    seedphrase: String,
    passphrase: String,
    external_index: u32,
    incoming_swapcoins: Vec<IncomingSwapCoin>,
    outgoing_swapcoins: Vec<OutgoingSwapCoin>,
    prevout_to_contract_map: HashMap<OutPoint, ScriptBuf>,
}

pub struct WalletStore {
    // Wallet store name should match the bitcoin core watch-only wallet name
    pub(crate) wallet_name: String,
    pub(crate) network: Network,
    pub(super) master_key: ExtendedPrivKey,
    pub(super) external_index: u32,
    pub(crate) offer_maxsize: u64,
    /// Map of multisig reedemscript to incoming swapcoins.
    pub(super) incoming_swapcoins: HashMap<ScriptBuf, IncomingSwapCoin>,
    /// Map of multisig reedemscript to outgoing swapcoins.
    pub(super) outgoing_swapcoins: HashMap<ScriptBuf, OutgoingSwapCoin>,
    pub(super) prevout_to_contract_map: HashMap<OutPoint, ScriptBuf>,
    pub(super) fidelity_scripts: HashMap<ScriptBuf, u32>,
    //TODO: Add last synced height and Wallet birthday.
}

impl TryFrom<FileData> for WalletStore {
    type Error = WalletError;
    fn try_from(file_data: FileData) -> Result<Self, WalletError> {
        let mnemonic = Mnemonic::parse(&file_data.seedphrase)?;
        let seed = mnemonic.to_seed(file_data.passphrase);
        let xprv = ExtendedPrivKey::new_master(file_data.network, &seed)?;

        let incoming_swapcoins = file_data
            .incoming_swapcoins
            .iter()
            .map(|sc| (sc.get_multisig_redeemscript(), sc.clone()))
            .collect::<HashMap<_, _>>();

        let outgoing_swapcoins = file_data
            .outgoing_swapcoins
            .iter()
            .map(|sc| (sc.get_multisig_redeemscript(), sc.clone()))
            .collect::<HashMap<_, _>>();

        let timelocked_script_index_map = generate_fidelity_scripts(&xprv);

        Ok(Self {
            wallet_name: file_data.wallet_name,
            network: file_data.network,
            master_key: xprv,
            external_index: file_data.external_index,
            incoming_swapcoins,
            outgoing_swapcoins,
            prevout_to_contract_map: file_data.prevout_to_contract_map,
            fidelity_scripts: timelocked_script_index_map,
            offer_maxsize: 0,
        })
    }
}

impl WalletStore {
    /// Initialize a store at a path. if path already exists, it will overwrite it.
    pub fn init(
        wallet_name: String,
        path: &PathBuf,
        network: Network,
        seedphrase: String,
        passphrase: String,
    ) -> Result<Self, WalletError> {
        FileData::init_new_file(path, wallet_name, network, seedphrase, passphrase)?;
        let store = WalletStore::read_from_disk(path)?;
        Ok(store)
    }

    /// Load existing file, updates it, writes it back.
    /// errors if path does not exist.
    pub fn write_to_disk(&self, path: &PathBuf) -> Result<(), WalletError> {
        let mut file_data = FileData::load_from_file(path)?;
        file_data.incoming_swapcoins = self
            .incoming_swapcoins
            .iter()
            .map(|is| is.1)
            .cloned()
            .collect();
        file_data.outgoing_swapcoins = self
            .outgoing_swapcoins
            .iter()
            .map(|os| os.1)
            .cloned()
            .collect();
        file_data.save_to_file(path)
    }

    /// Reads from a path. Errors if path doesn't exist.
    pub fn read_from_disk(path: &PathBuf) -> Result<Self, WalletError> {
        let file_data = FileData::load_from_file(path)?;
        Self::try_from(file_data)
    }
}

impl FileData {
    /// Overwrites existing file or create a new one.
    fn init_new_file(
        path: &PathBuf,
        wallet_name: String,
        network: Network,
        seedphrase: String,
        passphrase: String,
    ) -> Result<(), WalletError> {
        let file_data = Self {
            wallet_name,
            version: WALLET_FILE_VERSION,
            network,
            seedphrase,
            passphrase,
            external_index: 0,
            incoming_swapcoins: Vec::new(),
            outgoing_swapcoins: Vec::new(),
            prevout_to_contract_map: HashMap::new(),
        };
        file_data.save_to_file(path)
    }

    /// File path should exist.
    fn load_from_file(path: &PathBuf) -> Result<Self, WalletError> {
        let mut wallet_file = File::open(path)?;
        let mut wallet_file_str = String::new();
        wallet_file.read_to_string(&mut wallet_file_str)?;
        Ok(serde_json::from_str::<FileData>(&wallet_file_str)?)
    }

    // Overwrite existing file or create a new one.
    fn save_to_file(&self, path: &PathBuf) -> Result<(), WalletError> {
        std::fs::create_dir_all(path.parent().expect("path should not be root"))?;
        let file = OpenOptions::new().write(true).create(true).open(path)?;
        serde_json::to_writer(file, &self)?;
        Ok(())
    }
}

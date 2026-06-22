use std::{env, ffi::OsStr, fs, io::Write, path::PathBuf};

use crate::{
    security::{encrypt_struct, load_sensitive_struct, KeyMaterial, SerdeJson},
    wallet::{Wallet, WalletError},
};

use super::{
    rpc::{BackendConfig, BlockchainBackend},
    storage::WalletStore,
};
use bitcoin::{bip32::Xpriv, Network};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Represents a wallet backup, containing all the necessary information to
/// restore a wallet instance.
///
/// This struct captures the essential elements of a wallet's state, including
/// its network, master key, creation time, and file name. It is serializable
/// and can be persisted to disk or transferred for backup purposes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletBackup {
    /// Network the wallet operates on.
    pub(crate) network: Network, //Can be asked to the user, but is nice to save
    /// The master key for the wallet.
    pub(super) master_key: Xpriv,

    pub(super) wallet_birthday: Option<u64>, //Avoid scanning from genesis block
    /// The file name associated with the wallet store.
    pub file_name: String, //Can be asked to user, or stored for convenience
}
impl<B: BlockchainBackend> From<&Wallet<B>> for WalletBackup {
    fn from(wallet: &Wallet<B>) -> Self {
        WalletBackup {
            network: (wallet.store.network),
            master_key: (wallet.store.master_key),
            wallet_birthday: (wallet.store.wallet_birthday),
            file_name: (wallet.store.file_name.clone()),
        }
    }
}
impl<B: BlockchainBackend> Wallet<B> {
    /// Creates a backup of the wallet and writes it to the given path. The
    /// backup is saved as a `.json` file. If encryption material is provided,
    /// the backup content is encrypted before being written.
    pub fn backup(
        &self,
        path: &Path,
        backup_enc_material: Option<KeyMaterial>,
    ) -> Result<(), WalletError> {
        let mut backup_path = path.join("");
        backup_path.set_extension("json");

        log::info!("Backing up to {backup_path:?}");

        let backup = WalletBackup::from(self);

        let backup_file_content = match backup_enc_material {
            Some(key_material) => {
                let encrypted = encrypt_struct(backup, &key_material).unwrap();
                serde_json::to_string_pretty(&encrypted)?
            }
            None => {
                log::info!("Warning! The wallet backup file will be saved unencrypted!");
                serde_json::to_string_pretty(&backup)?
            }
        };
        let mut file = fs::File::create(backup_path)?;
        file.write_all(backup_file_content.as_bytes())?;

        Ok(())
    }

    /// Restore a wallet from a [`WalletBackup`]. The caller must set the wallet
    /// name in `config` to match `wallet_path.file_name()` before calling.
    pub fn restore(
        wallet_backup: &WalletBackup,
        wallet_path: &Path,
        config: &B::Config,
        restored_enc_material: Option<KeyMaterial>,
    ) -> Result<Self, WalletError> {
        let wallet_file_name = wallet_path
            .file_name()
            .unwrap_or(OsStr::new(&wallet_backup.file_name))
            .to_str()
            .unwrap()
            .to_string();
        let rpc = B::from_config(config)?;
        let store = WalletStore::init(
            wallet_file_name,
            wallet_path,
            wallet_backup.network,
            wallet_backup.master_key,
            wallet_backup.wallet_birthday,
            &restored_enc_material,
        )?;
        let mut tmp_wallet = Self {
            rpc,
            wallet_file_path: wallet_path.to_path_buf(),
            store,
            store_enc_material: restored_enc_material,
        };
        tmp_wallet.sync_and_save()?;
        Ok(tmp_wallet)
    }

    /// Interactive restore against the backend variant matching `B`. Prompts
    /// for an optional decryption passphrase, derives the wallet name from
    /// `restored_path`, and writes the restored wallet to disk.
    pub fn restore_interactive(
        backup_file_path: &PathBuf,
        backend: &BackendConfig,
        restored_path: &Path,
    ) {
        log::info!(
            "Initiating wallet restore, from backup: {backup_file_path:?} to wallet {:?}",
            restored_path.file_name()
        );

        let (backup, _) = load_sensitive_struct::<WalletBackup, SerdeJson>(backup_file_path, None);
        let restore_enc_material = KeyMaterial::new_interactive(Some(
            "Enter restored wallet encryption passphrase (empty for no encryption): ".to_string(),
        ));

        // Rebind the backend's wallet_name to the restored path's filename.
        // `Wallet::restore` doesn't rebind on its own.
        let mut backend = backend.clone();
        if let Some(name) = restored_path.file_name().and_then(|n| n.to_str()) {
            backend.set_wallet_name(name.to_string());
        }
        let cfg = match B::from_backend_config(&backend) {
            Ok(c) => c.clone(),
            Err(e) => {
                log::error!("Wallet restore failed: backend mismatch: {e:?}");
                return;
            }
        };

        if let Err(e) = Wallet::<B>::restore(&backup, restored_path, &cfg, restore_enc_material) {
            log::error!("Wallet restore failed: {e:?}");
        } else {
            println!("Wallet restore succeeded!");
        }
    }

    /// Interactively creates a wallet backup in the current working directory,
    /// optionally encrypted. File name is `{wallet_name}-backup.json`.
    pub fn backup_interactive(wallet: &Self, encrypt: bool) {
        log::info!("Initiating wallet backup!");
        let backup_name = format!("{}-backup", wallet.get_name());
        log::info!(
            "Backing up wallet: {} to {}",
            wallet.get_name(),
            backup_name
        );

        let working_directory: PathBuf =
            env::current_dir().expect("Failed to get current directory");

        let backup_enc_material = if encrypt {
            KeyMaterial::new_interactive(None)
        } else {
            None
        };

        let backup_path = working_directory.join(backup_name);
        if let Err(e) = wallet.backup(&backup_path, backup_enc_material) {
            log::error!("Wallet backup failed: {e:?}");
        } else {
            log::info!("Wallet backup succeeded: {backup_path:?}");
        }
    }
}

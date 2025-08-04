use std::{convert::TryFrom, env, ffi::OsStr, fs, io::Write, path::PathBuf};

use crate::{
    security::{encrypt_struct, load_sensitive_struct_interactive, KeyMaterial, SerdeJson},
    wallet::Wallet,
};

use super::{rpc::RPCConfig, storage::WalletStore};
use bitcoin::{bip32::Xpriv, Network};
use bitcoind::bitcoincore_rpc::Client;
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
impl From<&Wallet> for WalletBackup {
    fn from(wallet: &Wallet) -> Self {
        WalletBackup {
            network: (wallet.store.network),
            master_key: (wallet.store.master_key),
            wallet_birthday: (wallet.store.wallet_birthday),
            file_name: (wallet.store.file_name.clone()),
        }
    }
}
impl Wallet {
    /// Creates a backup of the wallet and writes it to the given path.
    ///
    /// The backup is saved as a `.json` file. If encryption material is provided,
    /// the backup content is encrypted before being written.
    ///
    /// # Behavior
    ///
    /// - If encryption is used, the backup content is encrypted and serialized.
    /// - If not, a warning is printed, and the backup is stored unencrypted.
    /// - The final backup file will have a `.json` extension.
    pub fn backup(&self, path: &Path, backup_enc_material: Option<KeyMaterial>) {
        let mut backup_path = path.join("");
        backup_path.set_extension("json");

        println!("Backing up to {backup_path:?}");

        let backup = WalletBackup::from(self);

        let backup_file_content = match backup_enc_material {
            Some(key_material) => {
                let encrypted = encrypt_struct(backup, &key_material).unwrap();
                serde_json::to_string_pretty(&encrypted).unwrap()
            }
            None => {
                println!("Warning! The wallet backup file will be saved unencrypted!");
                serde_json::to_string_pretty(&backup).unwrap()
            }
        };
        let mut file = fs::File::create(backup_path).unwrap();
        file.write_all(backup_file_content.as_bytes()).unwrap();
    }

    /// Restores a `Wallet` from this backup to a specified path.
    ///
    /// Initializes a new wallet instance using the data from the backup and syncs
    /// it with the blockchain using the provided RPC configuration.
    ///
    /// # Returns
    ///
    /// A fully initialized and synced `Wallet` instance.
    ///
    /// # Behavior
    ///
    /// If `wallet_path` does not contain a file name, `wallet_backup.file_name` will be used.
    /// The method initializes the wallet store, connects to the blockchain node via RPC,
    /// syncs wallet data, and saves the state to disk.
    pub fn restore(
        wallet_backup: &WalletBackup,
        wallet_path: &Path,
        rpc_config: &RPCConfig,
        restored_enc_material: Option<KeyMaterial>,
    ) -> Wallet {
        let wallet_file_name = wallet_path
            .file_name()
            .unwrap_or(OsStr::new(&wallet_backup.file_name)) // If no name filename for the restored one is provided use the previous one
            .to_str()
            .unwrap()
            .to_string();

        let mut rpc_config_test = rpc_config.clone();
        rpc_config_test.wallet_name = wallet_file_name.clone();

        let rpc = Client::try_from(&rpc_config_test).unwrap();

        // Initialise wallet
        let store = WalletStore::init(
            wallet_file_name,
            wallet_path,
            wallet_backup.network,
            wallet_backup.master_key,
            wallet_backup.wallet_birthday,
            &restored_enc_material,
        )
        .unwrap();

        let mut tmp_wallet = Wallet {
            rpc,
            wallet_file_path: wallet_path.to_path_buf(),
            store,
            store_enc_material: restored_enc_material,
        };
        tmp_wallet.sync().unwrap();
        tmp_wallet.save_to_disk().unwrap(); //Need to save after sync. due to offer_max_size not saving.
                                            //TODO check this final statements later
                                            //tmp_wallet.refresh_offer_maxsize_cache().unwrap();
        tmp_wallet
    }

    /// Interactively restores a wallet from a backup file.
    ///
    /// This method loads a wallet backup from the given file path, prompts for decryption
    /// if necessary, and then restores the wallet to a new location. During restoration,
    /// the user is also prompted to provide a new encryption passphrase for the restored wallet.
    ///
    /// # Behavior
    ///
    /// - **Prompts for decryption passphrase** if the backup file is encrypted.
    /// - Loads and decrypts the backup content.
    /// - **Prompts for a new encryption passphrase** for the restored wallet.
    /// - Initializes the wallet with the decrypted data and new encryption.
    /// - Syncs the wallet with the blockchain.
    /// - Saves the restored wallet to disk.
    pub fn restore_interactive(
        backup_file_path: &PathBuf,
        rpc_config: &RPCConfig,
        restored_path: &Path,
    ) {
        println!(
            "Initiating wallet restore, from backup: {backup_file_path:?} to wallet {:?}",
            restored_path.file_name()
        );

        let (backup, _) =
            load_sensitive_struct_interactive::<WalletBackup, SerdeJson>(backup_file_path);
        let restore_enc_material = KeyMaterial::new_interactive(Some(
            "Enter restored walled encryption passphrase(empty for no encryption): ".to_string(),
        ));

        Wallet::restore(&backup, restored_path, rpc_config, restore_enc_material);

        println!("Wallet Restore Ended!!");
    }
    /// Interactively creates a wallet backup, optionally encrypted.
    ///
    /// This is a user-friendly version of the [`backup`] method, which:
    /// - Uses the current working directory as the backup location.
    /// - Prompts the user to input encryption material (if `encrypt` is `true`).
    ///
    /// # Behavior
    ///
    /// - Prompts for encryption key if `encrypt == true`.
    /// - Names the backup file as `{wallet_name}-backup.json`.
    /// - Writes the backup to the current working directory.
    pub fn backup_interactive(wallet: &Self, encrypt: bool) {
        println!("Initiating wallet backup.");
        let backup_name = format!("{}-backup", wallet.get_name());
        println!(
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

        wallet.backup(&working_directory.join(backup_name), backup_enc_material);
        println!("Wallet Backup Ended");
    }
}

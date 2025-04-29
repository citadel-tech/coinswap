//! Maker Configuration. Controlling various behaviors.

use crate::utill::parse_toml;
use std::{io, path::Path};

use std::io::Write;

use crate::utill::{get_maker_dir, parse_field, ConnectionType};

use super::api::MIN_SWAP_AMOUNT;

/// Maker Configuration, controlling various maker behavior.
#[derive(Debug, Clone, PartialEq)]
pub struct MakerConfig {
    /// RPC listening port
    pub rpc_port: u16,
    /// Minimum Coinswap amount
    pub min_swap_amount: u64,
    /// Target listening port
    pub network_port: u16,
    /// Control port
    pub control_port: u16,
    /// Socks port
    pub socks_port: u16,
    /// Authentication password
    pub tor_auth_password: String,
    /// Directory server address (can be clearnet or onion)
    pub directory_server_address: String,
    /// Fidelity Bond amount
    pub fidelity_amount: u64,
    /// Fidelity Bond timelock in Block heights.
    pub fidelity_timelock: u32,
    /// Connection type
    pub connection_type: ConnectionType,
}

impl Default for MakerConfig {
    fn default() -> Self {
        Self {
            rpc_port: 6103,
            min_swap_amount: MIN_SWAP_AMOUNT,
            network_port: 6102,
            control_port: 9051,
            socks_port: 9050,
            tor_auth_password: "".to_string(),
            directory_server_address:
                "kizqnaslcb2r3mbk2vm77bdff3madcvddntmaaz2htmkyuw7sgh4ddqd.onion:8080".to_string(),
            #[cfg(feature = "integration-test")]
            fidelity_amount: 5_000_000, // 0.05 BTC for tests
            #[cfg(feature = "integration-test")]
            fidelity_timelock: 26_000, // Approx 6 months of blocks for test
            #[cfg(not(feature = "integration-test"))]
            fidelity_amount: 50_000, // 50K sats for production
            #[cfg(not(feature = "integration-test"))]
            fidelity_timelock: 13104, // Approx 3 months of blocks in production
            connection_type: if cfg!(feature = "integration-test") {
                ConnectionType::CLEARNET
            } else {
                ConnectionType::TOR
            },
        }
    }
}

impl MakerConfig {
    /// Constructs a [MakerConfig] from a specified data directory. Or creates default configs and load them.
    ///
    /// The maker(/taker).toml file should exist at the provided data-dir location.
    /// Or else, a new default-config will be loaded and created at the given data-dir location.
    /// If no data-dir is provided, a default config will be created at the default data-dir location.
    ///
    /// For reference of default config checkout `./maker.toml` in repo folder.
    ///
    /// Default data-dir for linux: `~/.coinswap/maker`
    /// Default config locations: `~/.coinswap/maker/config.toml`.
    pub(crate) fn new(config_path: Option<&Path>) -> io::Result<Self> {
        let default_config_path = get_maker_dir().join("config.toml");

        let config_path = config_path.unwrap_or(&default_config_path);
        let default_config = Self::default();

        // Creates a default config file at the specified path if it doesn't exist or is empty.
        if !config_path.exists() || std::fs::metadata(config_path)?.len() == 0 {
            log::warn!(
                "Maker config file not found, creating default config file at path: {}",
                config_path.display()
            );

            default_config.write_to_file(config_path)?;
        }

        let config_map = parse_toml(config_path)?;

        log::info!(
            "Successfully loaded config file from : {}",
            config_path.display()
        );

        Ok(MakerConfig {
            rpc_port: parse_field(config_map.get("rpc_port"), default_config.rpc_port),
            min_swap_amount: parse_field(
                config_map.get("min_swap_amount"),
                default_config.min_swap_amount,
            ),
            network_port: parse_field(config_map.get("network_port"), default_config.network_port),
            control_port: parse_field(config_map.get("control_port"), default_config.control_port),
            socks_port: parse_field(config_map.get("socks_port"), default_config.socks_port),
            tor_auth_password: parse_field(
                config_map.get("tor_auth_password"),
                default_config.tor_auth_password,
            ),
            directory_server_address: parse_field(
                config_map.get("directory_server_address"),
                default_config.directory_server_address,
            ),
            fidelity_amount: parse_field(
                config_map.get("fidelity_amount"),
                default_config.fidelity_amount,
            ),
            fidelity_timelock: parse_field(
                config_map.get("fidelity_timelock"),
                default_config.fidelity_timelock,
            ),
            connection_type: parse_field(
                config_map.get("connection_type"),
                default_config.connection_type,
            ),
        })
    }

    // Method to serialize the MakerConfig into a TOML string and write it to a file
    pub(crate) fn write_to_file(&self, path: &Path) -> std::io::Result<()> {
        let toml_data = format!(
            "network_port = {}
rpc_port = {}
socks_port = {}
control_port = {}
tor_auth_password = {}
min_swap_amount = {}
fidelity_amount = {}
fidelity_timelock = {}
connection_type = {:?}
directory_server_address = {}
",
            self.network_port,
            self.rpc_port,
            self.socks_port,
            self.control_port,
            self.tor_auth_password,
            self.min_swap_amount,
            self.fidelity_amount,
            self.fidelity_timelock,
            self.connection_type,
            self.directory_server_address,
        );

        std::fs::create_dir_all(path.parent().expect("Path should NOT be root!"))?;
        let mut file = std::fs::File::create(path)?;
        file.write_all(toml_data.as_bytes())?;
        // TODO: Why we do require Flush?
        file.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs::{self, File},
        io::Write,
        path::PathBuf,
    };

    fn create_temp_config(contents: &str, file_name: &str) -> PathBuf {
        let file_path = PathBuf::from(file_name);
        let mut file = File::create(&file_path).unwrap();
        writeln!(file, "{contents}").unwrap();
        file_path
    }

    fn remove_temp_config(path: &Path) {
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn test_valid_config() {
        let contents = r#"
            network_port = 6102
            rpc_port = 6103
            required_confirms = 1
            min_swap_amount = 10000
            socks_port = 9050
        "#;
        let config_path = create_temp_config(contents, "valid_maker_config.toml");
        let config = MakerConfig::new(Some(&config_path)).unwrap();
        remove_temp_config(&config_path);

        let default_config = MakerConfig::default();
        assert_eq!(config, default_config);
    }

    #[test]
    fn test_missing_fields() {
        let contents = r#"
            [maker_config]
            network_port = 6103
        "#;
        let config_path = create_temp_config(contents, "missing_fields_maker_config.toml");
        let config = MakerConfig::new(Some(&config_path)).unwrap();
        remove_temp_config(&config_path);

        assert_eq!(config.network_port, 6103);
        assert_eq!(
            MakerConfig {
                network_port: 6102,
                ..config
            },
            MakerConfig::default()
        );
    }

    #[test]
    fn test_incorrect_data_type() {
        let contents = r#"
            [maker_config]
            network_port = "not_a_number"
        "#;
        let config_path = create_temp_config(contents, "incorrect_type_maker_config.toml");
        let config = MakerConfig::new(Some(&config_path)).unwrap();
        remove_temp_config(&config_path);

        assert_eq!(config, MakerConfig::default());
    }

    #[test]
    fn test_missing_file() {
        let config_path = get_maker_dir().join("maker.toml");
        let config = MakerConfig::new(Some(&config_path)).unwrap();
        remove_temp_config(&config_path);
        assert_eq!(config, MakerConfig::default());
    }
}

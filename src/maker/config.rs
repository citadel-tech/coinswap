//! Maker Configuration. Controlling various Maker behaviors.
//!
//! This module defines the configuration options for the Maker server, controlling various aspects
//! of the maker's behavior including network settings, swap parameters, and security settings.

use crate::utill::parse_toml;
use std::{io, path::Path};

use std::io::Write;

use crate::utill::{get_maker_dir, parse_field};

use super::api::MIN_SWAP_AMOUNT;

/// Maximum and Minimum allowed timelocks for Fidelity Bonds
/// 1 block = ~10 minutes
const MIN_FIDELITY_TIMELOCK: u32 = 12_960;
const MAX_FIDELITY_TIMELOCK: u32 = 25_920;

/// Maker Configuration
///
/// This struct defines all configurable parameters for the Maker module, including:
/// - All the networking ports
/// - Swap amount limits
/// - Market settings, like Fidelity Bonds, Fidelity amount, etc.
/// - Connection preferences
#[derive(Debug, Clone, PartialEq)]
pub struct MakerConfig {
    /// RPC listening port for maker-cli operations
    pub rpc_port: u16,
    /// Network port for client connections
    pub network_port: u16,
    /// Control port for Tor interface
    pub control_port: u16,
    /// Socks port for Tor proxy
    pub socks_port: u16,
    /// Authentication password for Tor interface
    pub tor_auth_password: String,
    /// Minimum amount in satoshis that can be swapped
    pub min_swap_amount: u64,
    /// Fidelity Bond amount in satoshis
    pub fidelity_amount: u64,
    /// Fidelity Bond relative timelock in number of blocks
    pub fidelity_timelock: u32,
    /// A fixed base fee charged by the Maker for providing its services
    pub base_fee: u64,
    /// A percentage fee based on the swap amount.
    pub amount_relative_fee_pct: f64,
}

impl Default for MakerConfig {
    fn default() -> Self {
        let (fidelity_amount, fidelity_timelock, base_fee, amount_relative_fee_pct) =
            if cfg!(feature = "integration-test") {
                (5_000_000, 26_000, 1000, 2.50) // Test values
            } else {
                (50_000, 13104, 100, 0.1) // Production values
            };

        Self {
            rpc_port: 6103,
            min_swap_amount: MIN_SWAP_AMOUNT,
            network_port: 6102,
            control_port: 9051,
            socks_port: 9050,
            tor_auth_password: "".to_string(),
            fidelity_amount,
            fidelity_timelock,
            base_fee,
            amount_relative_fee_pct,
        }
    }
}

impl MakerConfig {
    /// Constructs a [`MakerConfig`] from a specified data directory. Or creates default configs and load them.
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
        let config = MakerConfig {
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
            fidelity_amount: parse_field(
                config_map.get("fidelity_amount"),
                default_config.fidelity_amount,
            ),
            fidelity_timelock: parse_field(
                config_map.get("fidelity_timelock"),
                default_config.fidelity_timelock,
            ),
            base_fee: parse_field(config_map.get("base_fee"), default_config.base_fee),
            amount_relative_fee_pct: parse_field(
                config_map.get("amount_relative_fee_pct"),
                default_config.amount_relative_fee_pct,
            ),
        };
        config
            .validate()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        Ok(config)
    }

    /// This function serializes the MakerConfig into a TOML format and writes it to disk.
    /// It creates the parent directory if it doesn't exist.
    pub(crate) fn write_to_file(&self, path: &Path) -> std::io::Result<()> {
        let toml_data = format!(
            "# Maker Configuration File
# Network port for client connections
network_port = {}
# RPC port for maker-cli operations
rpc_port = {}
# Socks port for Tor proxy
socks_port = {}
# Control port for Tor  interface
control_port = {}
# Authentication password for Tor interface
tor_auth_password = {}
# Minimum amount in satoshis that can be swapped
min_swap_amount = {}
# Fidelity Bond amount in satoshis
fidelity_amount = {}
# Fidelity Bond relative timelock in number of blocks 
fidelity_timelock = {}
# A fixed base fee charged by the Maker for providing its services (in satoshis)
base_fee = {}
# A percentage fee based on the swap amount
amount_relative_fee_pct = {}
",
            self.network_port,
            self.rpc_port,
            self.socks_port,
            self.control_port,
            self.tor_auth_password,
            self.min_swap_amount,
            self.fidelity_amount,
            self.fidelity_timelock,
            self.base_fee,
            self.amount_relative_fee_pct,
        );

        std::fs::create_dir_all(path.parent().expect("Path should NOT be root!"))?;
        let mut file = std::fs::File::create(path)?;
        file.write_all(toml_data.as_bytes())?;
        file.flush()?;
        Ok(())
    }

    /// Validates the MakerConfig parameters with bound checks.
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.fidelity_timelock < MIN_FIDELITY_TIMELOCK {
            return Err(format!(
                "Fidelity timelock too low: {}. Minimum is {} blocks.",
                self.fidelity_timelock, MIN_FIDELITY_TIMELOCK
            ));
        }
        if self.fidelity_timelock > MAX_FIDELITY_TIMELOCK {
            return Err(format!(
                "Fidelity timelock too high: {}. Maximum is {} blocks.",
                self.fidelity_timelock, MAX_FIDELITY_TIMELOCK
            ));
        }
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
    #[test]
    fn test_fidelity_timelock_validation_cases() {
        let cases = vec![
            (12_960, true),
            (15_000, true),
            (25_920, true),
            (12_000, false),
            (30_000, false),
        ];

        for (timelock, should_pass) in cases {
            let contents = format!("fidelity_timelock = {}", timelock);
            let path = create_temp_config(&contents, "timelock_test.toml");

            let result = MakerConfig::new(Some(&path));

            assert_eq!(
                result.is_ok(),
                should_pass,
                "timelock {} validation mismatch",
                timelock
            );

            remove_temp_config(&path);
        }
    }
}

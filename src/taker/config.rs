//! Taker configuration. Controlling various behaviors.
//!
//! This module defines the configuration options for the Taker module, controlling various aspects
//! of the taker's behavior including network settings, connection preferences, and security settings.

use crate::utill::{get_taker_dir, parse_field, parse_toml};
use std::{io, io::Write, path::Path};

/// Taker configuration
///
/// This struct defines all configurable parameters for the Taker app, including all network ports and marketplace settings
#[derive(Debug, Clone, PartialEq)]
pub struct TakerConfig {
    /// Control port for Tor interface (default: 9051)
    pub control_port: u16,
    /// Socks port for Tor proxy (default: 9050)
    pub socks_port: u16,
    /// Authentication password for Tor interface
    pub tor_auth_password: String,
    /// Tracker address for maker discovery
    pub tracker_address: String,
}

impl Default for TakerConfig {
    fn default() -> Self {
        Self {
            control_port: 9051,
            socks_port: 9050,
            tor_auth_password: "".to_string(),
            tracker_address: "lp75qh3del4qot6fmkqq4taqm33pidvk63lncvhlwsllbwrl2f4g4qqd.onion:8080"
                .to_string(),
        }
    }
}

impl TakerConfig {
    /// Constructs a [`TakerConfig`] from a specified data directory. Or create default configs and load them.
    ///
    /// The maker(/taker).toml file should exist at the provided data-dir location.
    /// Or else, a new default-config will be loaded and created at the given data-dir location.
    /// If no data-dir is provided, a default config will be created at the default data-dir location.
    ///
    /// For reference of default config checkout `./taker.toml` in repo folder.
    ///
    /// Default data-dir for linux: `~/.coinswap/taker`
    /// Default config locations: `~/.coinswap/taker/config.toml`.
    pub(crate) fn new(config_path: Option<&Path>) -> io::Result<Self> {
        let default_config_path = get_taker_dir().join("config.toml");

        let config_path = config_path.unwrap_or(&default_config_path);

        let default_config = Self::default();

        if !config_path.exists() || std::fs::metadata(config_path)?.len() == 0 {
            log::warn!(
                "Taker config file not found, creating default config file at path: {}",
                config_path.display()
            );
            default_config.write_to_file(config_path)?;
        }

        let config_map = parse_toml(config_path)?;

        log::info!(
            "Successfully loaded config file from : {}",
            config_path.display()
        );

        Ok(TakerConfig {
            control_port: parse_field(config_map.get("control_port"), default_config.control_port),
            socks_port: parse_field(config_map.get("socks_port"), default_config.socks_port),
            tor_auth_password: parse_field(
                config_map.get("tor_auth_password"),
                default_config.tor_auth_password,
            ),
            tracker_address: parse_field(
                config_map.get("tracker_address"),
                default_config.tracker_address,
            ),
        })
    }

    /// This method serializes the TakerConfig into a TOML format and writes it to disk.
    /// It creates the parent directory if it doesn't exist.
    pub(crate) fn write_to_file(&self, path: &Path) -> std::io::Result<()> {
        let toml_data = format!(
            "# Taker Configuration File
# Control port for Tor control interface
control_port = {}
# Socks port for Tor proxy
socks_port = {}
# Authentication password for Tor control interface
tor_auth_password = {}
# Tracker address
tracker_address = {}",
            self.control_port, self.socks_port, self.tor_auth_password, self.tracker_address,
        );

        std::fs::create_dir_all(path.parent().expect("Path should NOT be root!"))?;
        let mut file = std::fs::File::create(path)?;
        file.write_all(toml_data.as_bytes())?;
        file.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {

    use crate::taker::api::REFUND_LOCKTIME;

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
        control_port = 9051
        socks_port = 9050
        connection_type = "TOR"
        rpc_port = 8081
        "#;
        let config_path = create_temp_config(contents, "valid_taker_config.toml");
        let config = TakerConfig::new(Some(&config_path)).unwrap();
        remove_temp_config(&config_path);

        let default_config = TakerConfig::default();
        assert_eq!(config, default_config);
    }

    #[test]
    fn test_missing_fields() {
        let contents = r#"
            [taker_config]
            refund_locktime = 48
        "#;
        let config_path = create_temp_config(contents, "missing_fields_taker_config.toml");
        let config = TakerConfig::new(Some(&config_path)).unwrap();
        remove_temp_config(&config_path);

        assert_eq!(REFUND_LOCKTIME, 20);
        assert_eq!(config, TakerConfig::default());
    }

    #[test]
    fn test_incorrect_data_type() {
        let contents = r#"
            [taker_config]
            refund_locktime = "not_a_number"
        "#;
        let config_path = create_temp_config(contents, "incorrect_type_taker_config.toml");
        let config = TakerConfig::new(Some(&config_path)).unwrap();
        remove_temp_config(&config_path);

        assert_eq!(config, TakerConfig::default());
    }

    #[test]
    fn test_different_data() {
        let contents = r#"
            [taker_config]
            socks_port = 9050
        "#;
        let config_path = create_temp_config(contents, "different_data_taker_config.toml");
        let config = TakerConfig::new(Some(&config_path)).unwrap();
        remove_temp_config(&config_path);
        assert_eq!(REFUND_LOCKTIME, 20);
        assert_eq!(
            TakerConfig {
                socks_port: 9050,         // Configurable via TOML.
                ..TakerConfig::default()  // Use default for other values.
            },
            config
        );
    }

    #[test]
    fn test_missing_file() {
        let config_path = get_taker_dir().join("taker.toml");
        let config = TakerConfig::new(Some(&config_path)).unwrap();
        remove_temp_config(&config_path);
        assert_eq!(config, TakerConfig::default());
    }
}

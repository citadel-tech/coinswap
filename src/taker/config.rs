//! Taker configuration. Controlling various behavior.
//!
//!  Represents the configuration options for the Taker module, controlling behaviors
//! such as refund locktime, connection attempts, sleep delays, and timeouts.

use crate::utill::{get_taker_dir, parse_field, parse_toml, ConnectionType};
use std::{io, io::Write, path::PathBuf};

/// Taker configuration with refund, connection, and sleep settings.
#[derive(Debug, Clone, PartialEq)]
pub struct TakerConfig {
    /// Network listening port.
    /// This port is used by the Tor hidden service to bind and listen for incoming connections.  
    /// While the taker does not function as a traditional server, this port is crucial for  
    /// establishing a Tor hidden service. The Tor process leverages this port to create an  
    /// internal endpoint, enabling secure communication with peers, such as makers, over the  
    /// Tor network.  
    /// By binding to this port, the taker facilitates private and anonymous peer-to-peer (P2P)  
    /// interactions, preventing its real IP address from being exposed.
    pub port: u16,
    /// Socks port
    pub socks_port: u16,
    /// Directory server address (can be clearnet or onion)
    pub directory_server_address: String,
    /// Connection type
    pub connection_type: ConnectionType,
}

impl Default for TakerConfig {
    fn default() -> Self {
        Self {
            port: 8000,
            socks_port: 19050,
            directory_server_address: "directoryhiddenserviceaddress.onion:8080".to_string(),
            connection_type: ConnectionType::TOR,
        }
    }
}

impl TakerConfig {
    /// Constructs a [TakerConfig] from a specified data directory. Or create default configs and load them.
    ///
    /// The maker(/taker).toml file should exist at the provided data-dir location.
    /// Or else, a new default-config will be loaded and created at given data-dir location.
    /// If no data-dir is provided, a default config will be created at default data-dir location.
    ///
    /// For reference of default config checkout `./taker.toml` in repo folder.
    ///
    /// Default data-dir for linux: `~/.coinswap/taker`
    /// Default config locations: `~/.coinswap/taker/config.toml`.
    pub fn new(config_path: Option<&PathBuf>) -> io::Result<Self> {
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
            port: parse_field(config_map.get("port"), default_config.port),
            socks_port: parse_field(config_map.get("socks_port"), default_config.socks_port),
            directory_server_address: parse_field(
                config_map.get("directory_server_address"),
                default_config.directory_server_address,
            ),
            connection_type: parse_field(
                config_map.get("connection_type"),
                default_config.connection_type,
            ),
        })
    }

    // Method to manually serialize the Taker Config into a TOML string
    pub fn write_to_file(&self, path: &PathBuf) -> std::io::Result<()> {
        let toml_data = format!(
            "port = {}
socks_port = {}
directory_server_address = {}
connection_type = {:?}",
            self.port, self.socks_port, self.directory_server_address, self.connection_type
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
    };

    fn create_temp_config(contents: &str, file_name: &str) -> PathBuf {
        let file_path = PathBuf::from(file_name);
        let mut file = File::create(&file_path).unwrap();
        writeln!(file, "{}", contents).unwrap();
        file_path
    }

    fn remove_temp_config(path: &PathBuf) {
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn test_valid_config() {
        let contents = r#"
        port = 8000
        socks_port = 19050
        directory_server_address = directoryhiddenserviceaddress.onion:8080
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

        assert_eq!(REFUND_LOCKTIME, 48);
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
            socks_port = 19051
        "#;
        let config_path = create_temp_config(contents, "different_data_taker_config.toml");
        let config = TakerConfig::new(Some(&config_path)).unwrap();
        remove_temp_config(&config_path);
        assert_eq!(REFUND_LOCKTIME, 48);
        assert_eq!(
            TakerConfig {
                socks_port: 19051,        // Configurable via TOML.
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

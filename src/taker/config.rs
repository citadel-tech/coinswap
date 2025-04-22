//! Taker configuration. Controlling various behavior.
//!
//!  Represents the configuration options for the Taker module, controlling behaviors
//! such as refund locktime, connection attempts, sleep delays, and timeouts.

use crate::utill::{get_taker_dir, parse_field, parse_toml, ConnectionType};
use std::{io, io::Write, path::Path};

/// Taker configuration with refund, connection, and sleep settings.
#[derive(Debug, Clone, PartialEq)]
pub struct TakerConfig {
    /// Control port
    control_port: u16,
    /// Socks proxy port used to connect TOR
    socks_port: u16,
    /// Authentication password
    tor_auth_password: String,
    /// Directory server address (can be clearnet or onion)
    directory_server_address: String,
    /// Connection type
    connection_type: ConnectionType,
}

#[derive(Default, Clone, PartialEq)]
pub struct TakerConfigBuilder {
    /// Control port
    control_port: Option<u16>,
    /// Socks proxy port used to connect TOR
    socks_port: Option<u16>,
    /// Authentication password
    tor_auth_password: Option<String>,
    /// Directory server address (can be clearnet or onion)
    directory_server_address: Option<String>,
    /// Connection type
    connection_type: Option<ConnectionType>,
    // TakerConfig
    config: TakerConfig,
}

impl TakerConfigBuilder {
    /// Create a new TakerConfigBuilder with default values
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the control port
    pub fn control_port(mut self, control_port: u16) -> Self {
        self.control_port = Some(control_port);
        self
    }

    /// Set the socks port
    pub fn socks_port(mut self, socks_port: u16) -> Self {
        self.socks_port = Some(socks_port);
        self
    }

    /// Set the Tor authentication password
    pub fn tor_auth_password(mut self, tor_auth_password: impl Into<String>) -> Self {
        self.tor_auth_password = Some(tor_auth_password.into());
        self
    }

    /// Set the directory server address
    pub fn directory_server_address(mut self, directory_server_address: impl Into<String>) -> Self {
        self.directory_server_address = Some(directory_server_address.into());
        self
    }

    /// Set the connection type
    pub fn connection_type(mut self, connection_type: ConnectionType) -> Self {
        self.connection_type = Some(connection_type);
        self
    }

    /// Build the MakerConfig using the provided values or defaults
    pub fn build(self) -> TakerConfig {
        let default_config = TakerConfig::default();

        TakerConfig {
            control_port: self.control_port.unwrap_or(default_config.control_port),
            socks_port: self.socks_port.unwrap_or(default_config.socks_port),
            tor_auth_password: self
                .tor_auth_password
                .unwrap_or(default_config.tor_auth_password),
            directory_server_address: self
                .directory_server_address
                .unwrap_or(default_config.directory_server_address),
            connection_type: self
                .connection_type
                .unwrap_or(default_config.connection_type),
        }
    }

    // Load configuration from file
    pub fn from_file(mut self, config_path: Option<&Path>) -> io::Result<Self> {
        let config = TakerConfig::new(config_path)?;

        self.control_port = Some(config.control_port);
        self.socks_port = Some(config.socks_port);
        self.tor_auth_password = Some(config.tor_auth_password);
        self.directory_server_address = Some(config.directory_server_address);
        self.connection_type = Some(config.connection_type);

        Ok(self)
    }
}

impl Default for TakerConfig {
    fn default() -> Self {
        Self {
            control_port: 9051,
            socks_port: 9050,
            tor_auth_password: "".to_string(),
            directory_server_address:
                "kizqnaslcb2r3mbk2vm77bdff3madcvddntmaaz2htmkyuw7sgh4ddqd.onion:8080".to_string(),
            connection_type: if cfg!(feature = "integration-test") {
                ConnectionType::CLEARNET
            } else {
                ConnectionType::TOR
            },
        }
    }
}

impl TakerConfig {
    /// Creates a new builder for MakerConfig
    pub fn builder() -> TakerConfigBuilder {
        TakerConfigBuilder::new()
    }

    /// Get the the control port
    pub fn get_control_port(&self) -> u16 {
        self.control_port
    }

    /// Gets the configured SOCKS port used by Tor.
    pub fn get_socks_port(&self) -> u16 {
        self.socks_port
    }

    /// Returns the authentication password used for the Tor control port.
    pub fn get_tor_auth_password(&self) -> &str {
        &self.tor_auth_password
    }

    /// Returns the address of the configured directory server.
    pub fn get_directory_server_address(&self) -> &str {
        &self.directory_server_address
    }

    /// Returns the connection type used by the Maker.
    pub fn get_connection_type(&self) -> ConnectionType {
        self.connection_type
    }

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
    pub(crate) fn write_to_file(&self, path: &Path) -> std::io::Result<()> {
        let toml_data = format!(
            "control_port = {}
socks_port = {}
tor_auth_password = {}
directory_server_address = {}
connection_type = {:?}",
            self.control_port,
            self.socks_port,
            self.tor_auth_password,
            self.directory_server_address,
            self.connection_type
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
        writeln!(file, "{}", contents).unwrap();
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

    #[test]
    fn test_builder_pattern() {
        // Test building with default values
        let config = TakerConfig::builder().build();
        assert_eq!(config, TakerConfig::default());

        // Test modifying values using the builder
        let config = TakerConfig::builder()
            .control_port(9052)
            .socks_port(9053)
            .tor_auth_password("test_password")
            .directory_server_address("test.onion:8080")
            .connection_type(ConnectionType::CLEARNET)
            .build();

        let mut expected = TakerConfig::default();
        expected.control_port = 9052;
        expected.socks_port = 9053;
        expected.tor_auth_password = "test_password".to_owned();
        expected.directory_server_address = "test.onion:8080".to_owned();
        expected.connection_type = ConnectionType::CLEARNET;

        assert_eq!(config, expected);
    }
}

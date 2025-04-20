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
    rpc_port: u16,
    /// Minimum Coinswap amount
    min_swap_amount: u64,
    /// target listening port
    network_port: u16,
    /// control port
    control_port: u16,
    /// Socks port
    socks_port: u16,
    /// Authentication password
    tor_auth_password: String,
    /// Directory server address (can be clearnet or onion)
    directory_server_address: String,
    /// Fidelity Bond amount
    fidelity_amount: u64,
    /// Fidelity Bond timelock in Block heights.
    fidelity_timelock: u32,
    /// Connection type
    connection_type: ConnectionType,
}

#[derive(Default, Clone, PartialEq)]
pub struct MakerConfigBuilder {
    rpc_port: Option<u16>,
    /// Minimum Coinswap amount
    min_swap_amount: Option<u64>,
    /// target listening port
    network_port: Option<u16>,
    /// control port
    control_port: Option<u16>,
    /// Socks port
    socks_port: Option<u16>,
    /// Authentication password
    tor_auth_password: Option<String>,
    /// Directory server address (can be clearnet or onion)
    directory_server_address: Option<String>,
    /// Fidelity Bond amount
    fidelity_amount: Option<u64>,
    /// Fidelity Bond timelock in Block heights.
    fidelity_timelock: Option<u32>,
    /// Connection type
    connection_type: Option<ConnectionType>,
    // MakerConfig
    config: MakerConfig,
}

impl MakerConfigBuilder {
    /// Create a new MakerConfigBuilder with default values
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the RPC port
    pub fn rpc_port(mut self, rpc_port: u16) -> Self {
        self.rpc_port = Some(rpc_port);
        self
    }

    /// Set the minimum swap amount
    pub fn min_swap_amount(mut self, min_swap_amount: u64) -> Self {
        self.min_swap_amount = Some(min_swap_amount);
        self
    }

    /// Set the network port
    pub fn network_port(mut self, network_port: u16) -> Self {
        self.network_port = Some(network_port);
        self
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

    /// Set the fidelity bond amount
    pub fn fidelity_amount(mut self, fidelity_amount: u64) -> Self {
        self.fidelity_amount = Some(fidelity_amount);
        self
    }

    /// Set the fidelity timelock
    pub fn fidelity_timelock(mut self, fidelity_timelock: u32) -> Self {
        self.fidelity_timelock = Some(fidelity_timelock);
        self
    }

    /// Set the connection type
    pub fn connection_type(mut self, connection_type: ConnectionType) -> Self {
        self.connection_type = Some(connection_type);
        self
    }

    /// Build the MakerConfig using the provided values or defaults
    pub fn build(self) -> MakerConfig {
        let default_config = MakerConfig::default();

        MakerConfig {
            rpc_port: self.rpc_port.unwrap_or(default_config.rpc_port),
            min_swap_amount: self
                .min_swap_amount
                .unwrap_or(default_config.min_swap_amount),
            network_port: self.network_port.unwrap_or(default_config.network_port),
            control_port: self.control_port.unwrap_or(default_config.control_port),
            socks_port: self.socks_port.unwrap_or(default_config.socks_port),
            tor_auth_password: self
                .tor_auth_password
                .unwrap_or(default_config.tor_auth_password),
            directory_server_address: self
                .directory_server_address
                .unwrap_or(default_config.directory_server_address),
            fidelity_amount: self
                .fidelity_amount
                .unwrap_or(default_config.fidelity_amount),
            fidelity_timelock: self
                .fidelity_timelock
                .unwrap_or(default_config.fidelity_timelock),
            connection_type: self
                .connection_type
                .unwrap_or(default_config.connection_type),
        }
    }

    // Load configuration from file
    pub fn from_file(mut self, config_path: Option<&Path>) -> io::Result<Self> {
        let config = MakerConfig::new(config_path)?;

        self.rpc_port = Some(config.rpc_port);
        self.min_swap_amount = Some(config.min_swap_amount);
        self.network_port = Some(config.network_port);
        self.control_port = Some(config.control_port);
        self.socks_port = Some(config.socks_port);
        self.tor_auth_password = Some(config.tor_auth_password);
        self.directory_server_address = Some(config.directory_server_address);
        self.fidelity_amount = Some(config.fidelity_amount);
        self.fidelity_timelock = Some(config.fidelity_timelock);
        self.connection_type = Some(config.connection_type);

        Ok(self)
    }
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
    /// Creates a new builder for MakerConfig
    pub fn builder() -> MakerConfigBuilder {
        MakerConfigBuilder::new()
    }

    pub fn get_rpc_port(&self) -> u16 {
        self.rpc_port
    }

    pub fn get_min_swap_amount(&self) -> u64 {
        self.min_swap_amount
    }

    pub fn get_network_port(&self) -> u16 {
        self.network_port
    }

    pub fn get_control_port(&self) -> u16 {
        self.control_port
    }

    pub fn get_socks_port(&self) -> u16 {
        self.socks_port
    }

    pub fn get_tor_auth_password(&self) -> &str {
        &self.tor_auth_password
    }

    pub fn get_directory_server_address(&self) -> &str {
        &self.directory_server_address
    }

    pub fn get_fidelity_amount(&self) -> u64 {
        self.fidelity_amount
    }

    pub fn get_fidelity_timelock(&self) -> u32 {
        self.fidelity_timelock
    }

    pub fn get_connection_type(&self) -> ConnectionType {
        self.connection_type
    }

    /// Constructs a [MakerConfig] from a specified data directory. Or create default configs and load them.
    ///
    /// The maker(/taker).toml file should exist at the provided data-dir location.
    /// Or else, a new default-config will be loaded and created at given data-dir location.
    /// If no data-dir is provided, a default config will be created at default data-dir location.
    ///
    /// For reference of default config checkout `./maker.toml` in repo folder.
    ///
    /// Default data-dir for linux: `~/.coinswap/maker`
    /// Default config locations:`~/.coinswap/maker/config.toml`.
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
        writeln!(file, "{}", contents).unwrap();
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

    #[test]
    fn test_builder_pattern() {
        // Test building with default values
        let config = MakerConfig::builder().build();
        assert_eq!(config, MakerConfig::default());

        // Test modifying values using the builder
        let config = MakerConfig::builder()
            .rpc_port(7000)
            .network_port(7001)
            .control_port(9052)
            .socks_port(9053)
            .min_swap_amount(20000)
            .tor_auth_password("test_password")
            .directory_server_address("test.onion:8080")
            .fidelity_amount(100000)
            .fidelity_timelock(15000)
            .connection_type(ConnectionType::CLEARNET)
            .build();

        let mut expected = MakerConfig::default();
        expected.rpc_port = 7000;
        expected.network_port = 7001;
        expected.control_port = 9052;
        expected.socks_port = 9053;
        expected.min_swap_amount = 20000;
        expected.tor_auth_password = "test_password".to_owned();
        expected.directory_server_address = "test.onion:8080".to_owned();
        expected.fidelity_amount = 100000;
        expected.fidelity_timelock = 15000;
        expected.connection_type = ConnectionType::CLEARNET;

        assert_eq!(config, expected);
    }
}

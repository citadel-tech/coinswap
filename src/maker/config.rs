//! Maker Configuration. Controlling various behaviors.

use std::{io, path::PathBuf};

use bitcoin::Amount;
use std::io::Write;

use crate::utill::{get_maker_dir, parse_field, parse_toml, ConnectionType};

/// Maker Configuration, controlling various maker behavior.
#[derive(Debug, Clone, PartialEq)]
pub struct MakerConfig {
    /// Network listening port
    pub port: u16,
    /// RPC listening port
    pub rpc_port: u16,
    /// Absolute coinswap fee
    pub absolute_fee_sats: Amount,
    /// Fee rate per swap amount in ppb.
    pub amount_relative_fee_ppb: Amount,
    /// Fee rate for timelocked contract in ppb
    pub time_relative_fee_ppb: Amount,
    /// Minimum timelock difference between contract transaction of two hops
    pub min_contract_reaction_time: u16,
    /// Minimum coinswap amount size in sats
    pub min_size: u64,
    /// Socks port
    pub socks_port: u16,
    /// Directory server address (can be clearnet or onion)
    pub directory_server_address: String,
    /// Fidelity Bond Value
    pub fidelity_value: u64,
    /// Fidelity Bond timelock in Block heights.
    pub fidelity_timelock: u32,
    /// Connection type
    pub connection_type: ConnectionType,
}

impl Default for MakerConfig {
    fn default() -> Self {
        Self {
            port: 6102,
            rpc_port: 6103,
            absolute_fee_sats: Amount::from_sat(1000),
            amount_relative_fee_ppb: Amount::from_sat(10_000_000),
            time_relative_fee_ppb: Amount::from_sat(100_000),
            min_contract_reaction_time: 48,
            min_size: 10_000,
            socks_port: 19050,
            directory_server_address: "directoryhiddenserviceaddress.onion:8080".to_string(),
            fidelity_value: 5_000_000, // 5 million sats
            fidelity_timelock: 26_000, // Approx 6 months of blocks
            connection_type: ConnectionType::TOR,
        }
    }
}

impl MakerConfig {
    /// Constructs a [MakerConfig] from a specified data directory. Or create default configs and load them.
    ///
    /// The maker(/taker).toml file should exist at the provided data-dir location.
    /// Or else, a new default-config will be loaded and created at given data-dir location.
    /// If no data-dir is provided, a default config will be created at default data-dir location.
    ///
    /// For reference of default config checkout `./maker.toml` in repo folder.
    ///
    /// Default data-dir for linux: `~/.coinswap/`
    /// Default config locations, for taker for ex: `~/.coinswap/taker/config.toml`.
    pub fn new(config_path: Option<&PathBuf>) -> io::Result<Self> {
        let default_config = Self::default();

        let default_config_path = get_maker_dir().join("maker.toml");
        let config_path = config_path.unwrap_or(&default_config_path);

        if !config_path.exists() {
            write_default_maker_config(config_path);
            log::warn!(
                "Maker config file not found, creating default config file at path: {}",
                config_path.display()
            );
        }

        let section = parse_toml(config_path)?;
        log::info!(
            "Successfully loaded config file from : {}",
            config_path.display()
        );

        let maker_config_section = section.get("maker_config").cloned().unwrap_or_default();

        Ok(MakerConfig {
            port: parse_field(maker_config_section.get("port"), default_config.port)
                .unwrap_or(default_config.port),
            rpc_port: parse_field(
                maker_config_section.get("rpc_port"),
                default_config.rpc_port,
            )
            .unwrap_or(default_config.rpc_port),
            absolute_fee_sats: parse_field(
                maker_config_section.get("absolute_fee_sats"),
                default_config.absolute_fee_sats,
            )
            .unwrap_or(default_config.absolute_fee_sats),
            amount_relative_fee_ppb: parse_field(
                maker_config_section.get("amount_relative_fee_ppb"),
                default_config.amount_relative_fee_ppb,
            )
            .unwrap_or(default_config.amount_relative_fee_ppb),
            time_relative_fee_ppb: parse_field(
                maker_config_section.get("time_relative_fee_ppb"),
                default_config.time_relative_fee_ppb,
            )
            .unwrap_or(default_config.time_relative_fee_ppb),
            min_contract_reaction_time: parse_field(
                maker_config_section.get("min_contract_reaction_time"),
                default_config.min_contract_reaction_time,
            )
            .unwrap_or(default_config.min_contract_reaction_time),
            min_size: parse_field(
                maker_config_section.get("min_size"),
                default_config.min_size,
            )
            .unwrap_or(default_config.min_size),
            socks_port: parse_field(
                maker_config_section.get("socks_port"),
                default_config.socks_port,
            )
            .unwrap_or(default_config.socks_port),
            directory_server_address: maker_config_section
                .get("directory_server_onion_address")
                .map(|s| s.to_string())
                .unwrap_or(default_config.directory_server_address),
            fidelity_value: parse_field(
                maker_config_section.get("fidelity_value"),
                default_config.fidelity_value,
            )
            .unwrap_or(default_config.fidelity_value),
            fidelity_timelock: parse_field(
                maker_config_section.get("fidelity_timelock"),
                default_config.fidelity_timelock,
            )
            .unwrap_or(default_config.fidelity_timelock),
            connection_type: parse_field(
                maker_config_section.get("connection_type"),
                default_config.connection_type,
            )
            .unwrap_or(default_config.connection_type),
        })
    }

    // Method to serialize the MakerConfig into a TOML string and write it to a file
    pub fn write_to_file(&self, path: &PathBuf) -> std::io::Result<()> {
        let toml_data = format!(
            r#"
            port = {}
            rpc_port = {}
            absolute_fee_sats = {}
            amount_relative_fee_ppb = {}
            time_relative_fee_ppb = {}
            min_contract_reaction_time = {}
            min_size = {}
            socks_port = {}
            directory_server_address = "{}"
            fidelity_value = {}
            fidelity_timelock = {}
            connection_type = "{:?}"
            "#,
            self.port,
            self.rpc_port,
            self.absolute_fee_sats,
            self.amount_relative_fee_ppb,
            self.time_relative_fee_ppb,
            self.min_contract_reaction_time,
            self.min_size,
            self.socks_port,
            self.directory_server_address,
            self.fidelity_value,
            self.fidelity_timelock,
            self.connection_type,
        );

        std::fs::create_dir_all(path.parent().expect("Path should NOT be root!"))?;
        let mut file = std::fs::File::create(path)?;
        file.write_all(toml_data.as_bytes())?;
        file.flush()?;
        Ok(())
    }
}

fn write_default_maker_config(config_path: &PathBuf) {
    let config = MakerConfig::default();
    config.write_to_file(config_path).unwrap();
}

#[cfg(test)]
mod tests {
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
            [maker_config]
            port = 6102
            rpc_port = 6103
            absolute_fee_sats = 1000
            amount_relative_fee_ppb = 10000000
            time_relative_fee_ppb = 100000
            required_confirms = 1
            min_contract_reaction_time = 48
            min_size = 10000
            socks_port = 19050
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
            port = 6103
        "#;
        let config_path = create_temp_config(contents, "missing_fields_maker_config.toml");
        let config = MakerConfig::new(Some(&config_path)).unwrap();
        remove_temp_config(&config_path);

        assert_eq!(config.port, 6103);
        assert_eq!(
            MakerConfig {
                port: 6102,
                ..config
            },
            MakerConfig::default()
        );
    }

    #[test]
    fn test_incorrect_data_type() {
        let contents = r#"
            [maker_config]
            port = "not_a_number"
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

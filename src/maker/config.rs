//! Maker Configuration. Controlling various behaviors.

use std::{io, path::PathBuf};

use crate::utill::{get_maker_dir, parse_field, parse_toml, write_default_config, ConnectionType};

/// Maker Configuration, controlling various maker behavior.
#[derive(Debug, Clone, PartialEq)]
pub struct MakerConfig {
    /// Network listening port
    pub port: u16,
    /// RPC listening port
    pub rpc_port: u16,
    /// Time interval between connection checks
    pub heart_beat_interval_secs: u64,
    /// Time interval to ping the RPC backend
    pub rpc_ping_interval_secs: u64,
    /// Time interval ping directory server
    pub directory_servers_refresh_interval_secs: u64,
    /// Time interval to close a connection if no response is received
    pub idle_connection_timeout: u64,
    /// Absolute coinswap fee
    pub absolute_fee_sats: u64,
    /// Fee rate per swap amount in ppb.
    pub amount_relative_fee_ppb: u64,
    /// Fee rate for timelocked contract in ppb
    pub time_relative_fee_ppb: u64,
    /// No of confirmation required for funding transaction
    pub required_confirms: u64,
    // Minimum timelock difference between contract transaction of two hops
    pub min_contract_reaction_time: u16,
    /// Minimum coinswap amount size in sats
    pub min_size: u64,
    /// Socks port
    pub socks_port: u16,
    /// Directory server onion address
    pub directory_server_onion_address: String,
    /// Directory server clearnet address
    pub directory_server_clearnet_address: String,
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
            heart_beat_interval_secs: 3,
            rpc_ping_interval_secs: 60,
            directory_servers_refresh_interval_secs: 60 * 60 * 12, //12 Hours
            idle_connection_timeout: 300,
            absolute_fee_sats: 1000,
            amount_relative_fee_ppb: 10_000_000,
            time_relative_fee_ppb: 100_000,
            required_confirms: 1,
            min_contract_reaction_time: 48,
            min_size: 10_000,
            socks_port: 19050,
            directory_server_onion_address: "directoryhiddenserviceaddress.onion:8080".to_string(),
            directory_server_clearnet_address: "127.0.0.1:8080".to_string(),
            fidelity_value: 5_000_000, // 5 million  sats
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
            heart_beat_interval_secs: parse_field(
                maker_config_section.get("heart_beat_interval_secs"),
                default_config.heart_beat_interval_secs,
            )
            .unwrap_or(default_config.heart_beat_interval_secs),
            rpc_ping_interval_secs: parse_field(
                maker_config_section.get("rpc_ping_interval_secs"),
                default_config.rpc_ping_interval_secs,
            )
            .unwrap_or(default_config.rpc_ping_interval_secs),
            directory_servers_refresh_interval_secs: parse_field(
                maker_config_section.get("directory_servers_refresh_interval_secs"),
                default_config.directory_servers_refresh_interval_secs,
            )
            .unwrap_or(default_config.directory_servers_refresh_interval_secs),
            idle_connection_timeout: parse_field(
                maker_config_section.get("idle_connection_timeout"),
                default_config.idle_connection_timeout,
            )
            .unwrap_or(default_config.idle_connection_timeout),
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
            required_confirms: parse_field(
                maker_config_section.get("required_confirms"),
                default_config.required_confirms,
            )
            .unwrap_or(default_config.required_confirms),
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
            directory_server_onion_address: maker_config_section
                .get("directory_server_onion_address")
                .map(|s| s.to_string())
                .unwrap_or(default_config.directory_server_onion_address),
            directory_server_clearnet_address: maker_config_section
                .get("directory_server_clearnet_address")
                .map(|s| s.to_string())
                .unwrap_or(default_config.directory_server_clearnet_address),
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
}

fn write_default_maker_config(config_path: &PathBuf) {
    let config_string = String::from(
        "\
            [maker_config]\n\
            port = 6102\n\
            rpc_port = 6103\n\
            heart_beat_interval_secs = 3\n\
            rpc_ping_interval_secs = 60\n\
            directory_servers_refresh_interval_secs = 43200\n\
            idle_connection_timeout = 300\n\
            onion_addrs = myhiddenserviceaddress.onion\n\
            absolute_fee_sats = 1000\n\
            amount_relative_fee_ppb = 10000000\n\
            time_relative_fee_ppb = 100000\n\
            required_confirms = 1\n\
            min_contract_reaction_time = 48\n\
            min_size = 10000\n\
            socks_port = 19050\n\
            directory_server_onion_address = directoryhiddenserviceaddress.onion:8080\n\
            directory_server_clearnet_address = 127.0.0.1:8080\n\
            connection_type = tor
            ",
    );

    write_default_config(config_path, config_string).unwrap();
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
            heart_beat_interval_secs = 3
            rpc_ping_interval_secs = 60
            watchtower_ping_interval_secs = 300
            directory_servers_refresh_interval_secs = 43200
            idle_connection_timeout = 300
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

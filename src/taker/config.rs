//! Taker configuration. Controlling various behavior.
//!
//!  Represents the configuration options for the Taker module, controlling behaviors
//! such as refund locktime, connection attempts, sleep delays, and timeouts.

use std::{io, path::PathBuf};

use crate::utill::{get_taker_dir, parse_field, parse_toml, write_default_config, ConnectionType};
/// Taker configuration with refund, connection, and sleep settings.
#[derive(Debug, Clone, PartialEq)]
pub struct TakerConfig {
    pub refund_locktime: u16,
    pub refund_locktime_step: u16,

    pub first_connect_attempts: u32,
    pub first_connect_sleep_delay_sec: u64,
    pub first_connect_attempt_timeout_sec: u64,

    pub reconnect_attempts: u32,
    pub reconnect_short_sleep_delay: u64,
    pub reconnect_long_sleep_delay: u64,
    pub short_long_sleep_delay_transition: u32,
    pub reconnect_attempt_timeout_sec: u64,

    pub port: u16,
    pub socks_port: u16,
    pub directory_server_onion_address: String,
    pub directory_server_clearnet_address: String,
    pub connection_type: ConnectionType,
}

impl Default for TakerConfig {
    fn default() -> Self {
        Self {
            refund_locktime: 48,
            refund_locktime_step: 48,
            first_connect_attempts: 5,
            first_connect_sleep_delay_sec: 1,
            first_connect_attempt_timeout_sec: 60,
            reconnect_attempts: 3200,
            reconnect_short_sleep_delay: 10,
            reconnect_long_sleep_delay: 60,
            short_long_sleep_delay_transition: 60,
            reconnect_attempt_timeout_sec: 300,
            port: 8000,
            socks_port: 19050,
            directory_server_onion_address: "directoryhiddenserviceaddress.onion:8080".to_string(),
            directory_server_clearnet_address: "127.0.0.1:8080".to_string(),
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
    /// Default data-dir for linux: `~/.coinswap/`
    /// Default config locations: `~/.coinswap/taker/config.toml`.
    pub fn new(config_path: Option<&PathBuf>) -> io::Result<Self> {
        let default_config = Self::default();

        let default_config_path = get_taker_dir().join("config.toml");
        let config_path = config_path.unwrap_or(&default_config_path);

        if !config_path.exists() {
            write_default_taker_config(config_path);
            log::warn!(
                "Taker config file not found, creating default config file at path: {}",
                config_path.display()
            );
        }

        let section = parse_toml(config_path)?;
        log::info!(
            "Successfully loaded config file from : {}",
            config_path.display()
        );

        let taker_config_section = section.get("taker_config").cloned().unwrap_or_default();

        Ok(Self {
            refund_locktime: parse_field(
                taker_config_section.get("refund_locktime"),
                default_config.refund_locktime,
            )
            .unwrap_or(default_config.refund_locktime),
            refund_locktime_step: parse_field(
                taker_config_section.get("refund_locktime_step"),
                default_config.refund_locktime_step,
            )
            .unwrap_or(default_config.refund_locktime_step),
            first_connect_attempts: parse_field(
                taker_config_section.get("first_connect_attempts"),
                default_config.first_connect_attempts,
            )
            .unwrap_or(default_config.first_connect_attempts),
            first_connect_sleep_delay_sec: parse_field(
                taker_config_section.get("first_connect_sleep_delay_sec"),
                default_config.first_connect_sleep_delay_sec,
            )
            .unwrap_or(default_config.first_connect_sleep_delay_sec),
            first_connect_attempt_timeout_sec: parse_field(
                taker_config_section.get("first_connect_attempt_timeout_sec"),
                default_config.first_connect_attempt_timeout_sec,
            )
            .unwrap_or(default_config.first_connect_attempt_timeout_sec),
            reconnect_attempts: parse_field(
                taker_config_section.get("reconnect_attempts"),
                default_config.reconnect_attempts,
            )
            .unwrap_or(default_config.reconnect_attempts),
            reconnect_short_sleep_delay: parse_field(
                taker_config_section.get("reconnect_short_sleep_delay"),
                default_config.reconnect_short_sleep_delay,
            )
            .unwrap_or(default_config.reconnect_short_sleep_delay),
            reconnect_long_sleep_delay: parse_field(
                taker_config_section.get("reconnect_long_sleep_delay"),
                default_config.reconnect_long_sleep_delay,
            )
            .unwrap_or(default_config.reconnect_long_sleep_delay),
            short_long_sleep_delay_transition: parse_field(
                taker_config_section.get("short_long_sleep_delay_transition"),
                default_config.short_long_sleep_delay_transition,
            )
            .unwrap_or(default_config.short_long_sleep_delay_transition),
            reconnect_attempt_timeout_sec: parse_field(
                taker_config_section.get("reconnect_attempt_timeout_sec"),
                default_config.reconnect_attempt_timeout_sec,
            )
            .unwrap_or(default_config.reconnect_attempt_timeout_sec),
            port: parse_field(taker_config_section.get("port"), default_config.port)
                .unwrap_or(default_config.port),
            socks_port: parse_field(
                taker_config_section.get("socks_port"),
                default_config.socks_port,
            )
            .unwrap_or(default_config.socks_port),
            directory_server_onion_address: taker_config_section
                .get("directory_server_onion_address")
                .map(|s| s.to_string())
                .unwrap_or(default_config.directory_server_onion_address),
            directory_server_clearnet_address: taker_config_section
                .get("directory_server_clearnet_address")
                .map(|s| s.to_string())
                .unwrap_or(default_config.directory_server_clearnet_address),
            connection_type: parse_field(
                taker_config_section.get("connection_type"),
                default_config.connection_type,
            )
            .unwrap_or(default_config.connection_type),
        })
    }
}

fn write_default_taker_config(config_path: &PathBuf) {
    let config_string = String::from(
        "\
                        [taker_config]\n\
                        refund_locktime = 48\n\
                        refund_locktime_step = 48\n\
                        first_connect_attempts = 5\n\
                        first_connect_sleep_delay_sec = 1\n\
                        first_connect_attempt_timeout_sec = 60\n\
                        reconnect_attempts = 3200\n\
                        reconnect_short_sleep_delay = 10\n\
                        reconnect_long_sleep_delay = 60\n\
                        short_long_sleep_delay_transition = 60\n\
                        reconnect_attempt_timeout_sec = 300\n\
                        port = 8000\n\
                        socks_port = 19050\n\
                        directory_server_onion_address = directoryhiddenserviceaddress.onion:8080\n\
                        directory_server_clearnet_address = 127.0.0.1:8080\n\
                        connection_type = tor\n
                        ",
    );
    write_default_config(config_path, config_string).unwrap();
}

#[cfg(test)]
mod tests {
    use crate::utill::get_taker_dir;

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
        [taker_config]
        refund_locktime = 48
        refund_locktime_step = 48
        first_connect_attempts = 5
        first_connect_sleep_delay_sec = 1
        first_connect_attempt_timeout_sec = 60
        reconnect_attempts = 3200
        reconnect_short_sleep_delay = 10
        reconnect_long_sleep_delay = 60
        short_long_sleep_delay_transition = 60
        reconnect_attempt_timeout_sec = 300
        port = 8000
        socks_port = 19050
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

        assert_eq!(config.refund_locktime, 48);
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
            refund_locktime = 49
        "#;
        let config_path = create_temp_config(contents, "different_data_taker_config.toml");
        let config = TakerConfig::new(Some(&config_path)).unwrap();
        remove_temp_config(&config_path);
        assert_eq!(config.refund_locktime, 49);
        assert_eq!(
            TakerConfig {
                refund_locktime: 48,
                ..config
            },
            TakerConfig::default()
        )
    }

    #[test]
    fn test_missing_file() {
        let config_path = get_taker_dir().join("taker.toml");
        let config = TakerConfig::new(Some(&config_path)).unwrap();
        remove_temp_config(&config_path);
        assert_eq!(config, TakerConfig::default());
    }
}

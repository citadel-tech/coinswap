//! TOR spawning module
//!
//! This module provides functionality for managing TOR instances using mitosis for spawning and
//! handling processes. It includes utilities to initialize mitosis, spawn TOR processes, and
//! gracefully terminate them.
use std::{
    io::{BufRead, BufReader},
    process::{Child, Command},
};

use libtor::{HiddenServiceVersion, LogDestination, LogLevel, Tor, TorAddress, TorFlag};

/// Used as the main function in tor binary
pub fn start_tor(socks_port: u16, port: u16, base_dir: String) -> Result<(), libtor::Error> {
    let hs_string = format!("{}/hs-dir/", base_dir);
    let data_dir = format!("{}/", base_dir);
    let log_file = format!("{}/log", base_dir);
    Tor::new()
        .flag(TorFlag::DataDirectory(data_dir))
        .flag(TorFlag::LogTo(
            LogLevel::Notice,
            LogDestination::File(log_file),
        ))
        .flag(TorFlag::SocksPort(socks_port))
        .flag(TorFlag::HiddenServiceDir(hs_string))
        .flag(TorFlag::HiddenServiceVersion(HiddenServiceVersion::V3))
        .flag(TorFlag::HiddenServicePort(
            TorAddress::Port(port),
            None.into(),
        ))
        .start()?;
    Ok(())
}

/// Used to programmatically spawn tor process in maker, taker, and dns.
pub fn spawn_tor(socks_port: u16, port: u16, base_dir: String) -> Result<Child, std::io::Error> {
    let mut tor_process = Command::new("./target/debug/tor")
        .args([
            "-s",
            &socks_port.to_string(),
            "-p",
            &port.to_string(),
            "-d",
            &base_dir,
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    let stdout = tor_process.stdout.take().unwrap();
    let stderr = tor_process.stderr.take().unwrap();

    // Spawn threads to capture stdout and stderr.
    std::thread::spawn(move || {
        let reader = BufReader::new(stderr);
        if let Some(line) = reader.lines().map_while(Result::ok).next() {
            log::info!("{}", line);
        }
    });

    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            log::info!("{}", line);
        }
    });

    Ok(tor_process)
}

/// Kills all the tor processes.
pub fn kill_tor_handles(handle: &mut Child) {
    match handle.kill().and_then(|_| handle.wait()) {
        Ok(_) => log::info!("Tor instance terminated successfully"),
        Err(e) => log::error!("Error occurred while terminating tor instance {:?}", e),
    };
}

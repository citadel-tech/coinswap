use libtor::{HiddenServiceVersion, LogDestination, LogLevel, Tor, TorAddress, TorFlag};
use mitosis::JoinHandle;

pub fn setup_mitosis() {
    mitosis::init();
}

pub fn spawn_tor(socks_port: u16, port: u16, base_dir: String) -> JoinHandle<()> {
    let handle = mitosis::spawn(
        (socks_port, port, base_dir),
        |(socks_port, port, base_dir)| {
            let hs_string = format!("{}/hs-dir/", base_dir);
            let data_dir = format!("{}/", base_dir);
            let log_dir = format!("{}/log", base_dir);
            let _handler = Tor::new()
                .flag(TorFlag::DataDirectory(data_dir))
                .flag(TorFlag::LogTo(
                    LogLevel::Notice,
                    LogDestination::File(log_dir),
                ))
                .flag(TorFlag::SocksPort(socks_port))
                .flag(TorFlag::HiddenServiceDir(hs_string))
                .flag(TorFlag::HiddenServiceVersion(HiddenServiceVersion::V3))
                .flag(TorFlag::HiddenServicePort(
                    TorAddress::Port(port),
                    None.into(),
                ))
                .start();
        },
    );

    handle
}

pub fn kill_tor_handles(handle: JoinHandle<()>) {
    match handle.kill() {
        Ok(_) => log::info!("Tor instance terminated successfully"),
        Err(_) => log::error!("Error occurred while terminating tor instance"),
    };
}

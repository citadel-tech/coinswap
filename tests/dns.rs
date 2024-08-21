use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, RwLock};
use std::thread;

#[test]
fn test_dns() {
    // Setup the directory server's data directory with default configs
    let config_path = PathBuf::from("./.cargo/coinswap-test-data/directory/config.toml");
    fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    fs::write(
        &config_path,
        "\
        [directory_config]\n\
        port = 8080\n\
        socks_port = 19060\n\
        connection_type = tor\n\
        rpc_port = 4321\n\
        ",
    )
    .unwrap();

    // spawn the directoryd thread and get the log via mpsc channel.
    let _directoryd_process = Command::new("cargo")
        .args(["run", "--bin", "directoryd"])
        .spawn()
        .expect("Failed to start directoryd process");

    thread::sleep(std::time::Duration::from_secs(2));

    let addresses: Arc<RwLock<HashSet<String>>> = Arc::new(RwLock::new(HashSet::new()));

    let address = addresses.read().unwrap().iter().next().cloned().unwrap();

    // assert that address is of specific value.
    let expected_address = "http://localhost:8080";
    assert_eq!(address, expected_address);
}

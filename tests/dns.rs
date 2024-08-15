use coinswap::market::directory::{start_directory_server, DirectoryServer};
use coinswap::utill::ConnectionType;
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, RwLock};
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

    let directory_server_instance = Arc::new(
        DirectoryServer::new(Some(config_path.clone()), Some(ConnectionType::CLEARNET)).unwrap(),
    );
    let (tx, rx) = mpsc::channel();

    let directory_server_instance_clone = directory_server_instance.clone();

    thread::spawn(move || {
        start_directory_server(directory_server_instance_clone);
        tx.send("Server started".to_string()).unwrap();
    });

    thread::sleep(std::time::Duration::from_secs(2));

    match rx.recv() {
        Ok(log_message) => println!("{}", log_message),
        Err(e) => eprintln!("Failed to receive log message: {}", e),
    }

    // get the address of the server (local/tor).
    let addresses: Arc<RwLock<HashSet<String>>> = Arc::new(RwLock::new(HashSet::new()));

    let address = addresses.read().unwrap().iter().next().cloned().unwrap();

    // assert that address is of specific value.
    let expected_address = "http://localhost:8080"; // Replace with the actual expected address
    assert_eq!(address, expected_address);
}

use std::{
    path::PathBuf,
    process::Command,
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::Duration,
};

#[test]
fn test_dns() {
    let config_path = PathBuf::from("./.cargo/coinswap-test-data/directory/config.toml");

    let (_log_sender, log_receiver): (Sender<String>, Receiver<String>) = mpsc::channel();

    let mut directoryd_process = Command::new("./target/debug/directoryd")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let mut server_started = false;
    while let Ok(log_message) = log_receiver.recv_timeout(Duration::from_secs(5)) {
        if log_message.contains("RPC socket binding successful") {
            server_started = true;
            break;
        }
    }
    assert!(
        server_started,
        "Server did not start within the expected time"
    );
    thread::sleep(Duration::from_secs(5));

    let output = Command::new("./target/debug/directory-cli")
        .args("ListAddresses")
        .output()
        .unwrap();

    let addresses: Vec<String> = serde_json::from_slice(&output.stdout).unwrap();

    directoryd_process
        .kill()
        .expect("Failed to kill directoryd process");
}

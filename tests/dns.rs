#![cfg(feature = "integration-test")]
use std::{io::Write, net::TcpStream, process::Command, thread, time::Duration};

mod test_framework;

use coinswap::utill::DnsRequest;
use test_framework::{init_bitcoind, start_dns};

fn send_addresses(addresses: &[&str]) {
    for address in addresses {
        let mut stream = TcpStream::connect(("127.0.0.1", 8080)).unwrap();
        let request = DnsRequest::Dummy {
            url: address.to_string(),
        };
        let buffer = serde_cbor::ser::to_vec(&request).unwrap();
        let length = buffer.len() as u32;
        stream.write_all(&length.to_be_bytes()).unwrap();
        stream.write_all(&buffer).unwrap();
        stream.flush().unwrap();
    }
}

fn verify_addresses(addresses: &[&str]) {
    let output = Command::new("./target/debug/directory-cli")
        .arg("list-addresses")
        .output()
        .unwrap();
    let addresses_output = String::from_utf8(output.stdout).unwrap();

    assert!(
        output.stderr.is_empty(),
        "Error: {:?}",
        String::from_utf8(output.stderr).unwrap()
    );

    for address in addresses {
        assert!(
            addresses_output.contains(&address.to_string()),
            "Address {} not found",
            address
        );
    }
}

#[test]
fn test_dns() {
    // Setup directory
    let temp_dir = std::env::temp_dir().join("coinswap");
    // Remove if previously existing
    if temp_dir.exists() {
        std::fs::remove_dir_all(&temp_dir).unwrap();
    }
    log::info!("temporary directory : {}", temp_dir.display());

    let bitcoind = init_bitcoind(&temp_dir);

    let data_dir = temp_dir.join("dns");

    let mut process = start_dns(&data_dir, &bitcoind);

    let initial_addresses = vec!["127.0.0.1:8080", "127.0.0.1:8081", "127.0.0.1:8082"];
    send_addresses(&initial_addresses);
    thread::sleep(Duration::from_secs(10));
    verify_addresses(&initial_addresses);

    // Persistence check
    process.kill().expect("Failed to kill directoryd process");
    process.wait().unwrap();

    let mut process = start_dns(&data_dir, &bitcoind);

    let additional_addresses = vec!["127.0.0.1:8083", "127.0.0.1:8084"];
    send_addresses(&additional_addresses);
    thread::sleep(Duration::from_secs(10));

    process.kill().expect("Failed to kill directoryd process");
    process.wait().unwrap();

    let mut process = start_dns(&data_dir, &bitcoind);
    let all_addresses = vec![
        "127.0.0.1:8080",
        "127.0.0.1:8081",
        "127.0.0.1:8082",
        "127.0.0.1:8083",
        "127.0.0.1:8084",
    ];
    verify_addresses(&all_addresses);

    process.kill().expect("Failed to kill directoryd process");
    process.wait().unwrap();
}

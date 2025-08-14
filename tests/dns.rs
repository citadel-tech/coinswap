#![cfg(feature = "integration-test")]
use std::{io::Write, net::TcpStream, process::Command, thread, time::Duration};

mod test_framework;

use coinswap::protocol::DnsRequest;
use log::info;
use test_framework::{init_bitcoind, start_dns};

fn send_addresses(addresses: &[(&str, u32)]) {
    for address in addresses {
        let mut stream = TcpStream::connect(("127.0.0.1", 8080)).unwrap();
        let request = DnsRequest::Dummy {
            url: address.0.to_string(),
            vout: address.1,
        };
        let buffer = serde_cbor::ser::to_vec(&request).unwrap();
        let length = buffer.len() as u32;
        stream.write_all(&length.to_be_bytes()).unwrap();
        stream.write_all(&buffer).unwrap();
        stream.flush().unwrap();
    }
}

fn verify_addresses(addresses: &[(&str, u32)]) {
    let output = Command::new(env!("CARGO_BIN_EXE_directory-cli"))
        .arg("list-addresses")
        .output()
        .unwrap();
    let addresses_output = String::from_utf8(output.stdout).unwrap();

    println!("{addresses_output}");

    assert!(
        output.stderr.is_empty(),
        "Error: {:?}",
        String::from_utf8(output.stderr).unwrap()
    );

    for (address, index) in addresses {
        assert_eq!(
            addresses_output.match_indices(&address.to_string()).count(),
            1,
            "Address {address} not found or duplicate entries found",
        );
        assert_eq!(
            addresses_output
                .match_indices(&format!("vout: {index}"))
                .count(),
            1,
            "OP index {address} not found",
        );
    }
}

#[ignore]
#[test]
fn test_dns() {
    info!("🧪 Running Test: DNS server address registration and management");

    // Setup directory
    let temp_dir = std::env::temp_dir().join("coinswap");
    // Remove if previously existing
    if temp_dir.exists() {
        std::fs::remove_dir_all(&temp_dir).unwrap();
    }
    log::info!("temporary directory : {}", temp_dir.display());

    let bitcoind = init_bitcoind(&temp_dir);

    let data_dir = temp_dir.join("dns");

    info!("🚀 Starting DNS server");
    let mut process = start_dns(&data_dir, &bitcoind);

    info!("📤 Sending initial addresses to DNS server");
    // The indexes denotes vout of an `OutPoint(deadbeefcafebabefeedc0ffee123456789abcdeffedcba9876543210ffeeddcc:vout)``
    // So using the same index for different address, will replace the address.
    let initial_addresses = vec![
        ("127.0.0.1:8080", 0),
        ("127.0.0.1:8081", 1),
        ("127.0.0.1:8082", 2),
    ];
    send_addresses(&initial_addresses);
    thread::sleep(Duration::from_secs(10));

    info!("🔍 Verifying initial address registration");
    verify_addresses(&initial_addresses);

    info!("🔄 Testing address replacement and new registration");
    // Replace address 8082 to 8083 registered for Bond index 2.
    // Add a new entry with a new bond index
    let additional_addresses = vec![("127.0.0.1:8083", 2), ("127.0.0.1:8084", 3)];
    send_addresses(&additional_addresses);
    thread::sleep(Duration::from_secs(10));

    info!("📊 Verifying updated address list");
    let all_addresses = vec![
        ("127.0.0.1:8080", 0),
        ("127.0.0.1:8081", 1),
        ("127.0.0.1:8083", 2),
        ("127.0.0.1:8084", 3),
    ];
    verify_addresses(&all_addresses);

    info!("🔧 Testing DNS server shutdown");
    // Persistence check
    process.kill().expect("Failed to kill directoryd process");
    process.wait().unwrap();

    info!("🎉 DNS test completed successfully");
}

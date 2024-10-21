use std::{io::Write, net::TcpStream, process::Command, thread, time::Duration};

#[test]
fn test_dns() {
    let mut directoryd_process = Command::new("./target/debug/directoryd")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    thread::sleep(Duration::from_secs(30));

    let test_addresses = vec!["127.0.0.1:8080", "127.0.0.1:8081", "127.0.0.1:8082"];
    for address in &test_addresses {
        let mut stream = TcpStream::connect(("127.0.0.1", 4321)).unwrap();
        let request = format!("POST {}\n", address);
        stream.write_all(request.as_bytes()).unwrap();
    }

    let output = Command::new("./target/debug/directory-cli")
        .arg("ListAddresses")
        .output()
        .unwrap();

    let addresses: Vec<String> = serde_json::from_slice(&output.stdout).unwrap();

    for address in test_addresses {
        assert!(
            addresses.contains(&address.to_string()),
            "Address {} not found",
            address
        );
    }

    directoryd_process
        .kill()
        .expect("Failed to kill directoryd process");
}

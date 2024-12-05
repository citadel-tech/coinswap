#![cfg(feature = "integration-test")]
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpStream,
    process::{Child, Command},
    sync::{
        mpsc,
        mpsc::{Receiver, Sender},
    },
    thread,
    time::Duration,
};

fn start_server() -> (Child, Receiver<String>) {
    let (log_sender, log_receiver): (Sender<String>, Receiver<String>) = mpsc::channel();
    let mut directoryd_process = Command::new("./target/debug/directoryd")
        .args(["-n", "clearnet"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let stdout = directoryd_process.stdout.take().unwrap();
    let std_err = directoryd_process.stderr.take().unwrap();
    thread::spawn(move || {
        let reader = BufReader::new(stdout);
        reader.lines().map_while(Result::ok).for_each(|line| {
            println!("{}", line);
            log_sender.send(line).unwrap_or_else(|e| {
                println!("Failed to send log: {}", e);
            });
        });
    });

    thread::spawn(move || {
        let reader = BufReader::new(std_err);
        reader.lines().map_while(Result::ok).for_each(|line| {
            panic!("Error : {}", line);
        })
    });

    (directoryd_process, log_receiver)
}

fn wait_for_server_start(log_receiver: &Receiver<String>) {
    loop {
        let log_message = log_receiver.recv().unwrap();
        if log_message.contains("RPC socket binding successful") {
            log::info!("DNS server started");
            break;
        }
    }
}

fn send_addresses(addresses: &[&str]) {
    for address in addresses {
        let mut stream = TcpStream::connect(("127.0.0.1", 8080)).unwrap();
        let request = format!("POST {}\n", address);
        stream.write_all(request.as_bytes()).unwrap();
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
    let (mut process, receiver) = start_server();
    wait_for_server_start(&receiver);

    let initial_addresses = vec!["127.0.0.1:8080", "127.0.0.1:8081", "127.0.0.1:8082"];
    send_addresses(&initial_addresses);
    thread::sleep(Duration::from_secs(10));
    verify_addresses(&initial_addresses);

    // Persistence check
    process.kill().expect("Failed to kill directoryd process");
    process.wait().unwrap();

    let (mut process, receiver) = start_server();
    wait_for_server_start(&receiver);

    let additional_addresses = vec!["127.0.0.1:8083", "127.0.0.1:8084"];
    send_addresses(&additional_addresses);
    thread::sleep(Duration::from_secs(10));

    process.kill().expect("Failed to kill directoryd process");
    process.wait().unwrap();

    let (mut process, receiver) = start_server();
    wait_for_server_start(&receiver);

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

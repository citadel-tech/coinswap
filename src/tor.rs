use libtor::{HiddenServiceVersion, Tor, TorAddress, TorFlag};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
const SOCKS_PORT: u16 = 9002;
const CHECK_URL: &str = "https://check.torproject.org";

fn tor_instance() {
    match Tor::new()
        .flag(TorFlag::DataDirectory("/tmp/tor-rust".into()))
        .flag(TorFlag::SocksPort(SOCKS_PORT))
        .flag(TorFlag::HiddenServiceDir("/tmp/tor-rust/hs-dir".into()))
        .flag(TorFlag::HiddenServiceVersion(HiddenServiceVersion::V3))
        .flag(TorFlag::HiddenServicePort(
            TorAddress::Port(8000),
            None.into(),
        ))
        .start()
    {
        Ok(r) => println!("tor exit result: {}", r),
        Err(e) => println!("tor error: {}", e),
    };
}

#[cfg(test)]
mod test {
    use std::{thread::{self, JoinHandle}, time::Duration, fs::File, io::Read, net::TcpStream};

    use bitcoin::Network;
    use socks::{Socks5Stream, TargetAddr};
    use tokio::{task, io::{BufStream, AsyncWriteExt}, net::TcpListener};
    use crate::tor::*;
    use crate::market::directory::post_maker_address_to_directory_servers;
    async fn run_server() -> std::io::Result<()> {

        println!("I am here as well");
    
        let mut file = File::open("/home/admin123/maker/tmp/tor-rust/hs-dir/hostname").unwrap();
        let mut buffer = String::new();

        // Read the file contents into the buffer
        let value = file.read_to_string(&mut buffer).unwrap(); 
        let onion_addr = buffer.trim();
        println!("{:?}",onion_addr);

        let listener = TcpListener::bind("127.0.0.1:8000").await.unwrap();
        println!("Listening on 127.0.0.1:8000");

        loop {
            let (mut socket, addr) = listener.accept().await.unwrap();
            println!("Connection received from {:?}", addr);

            // Spawn a new task for each connection
            task::spawn(async move {
                let (reader, mut writer) = socket.split();
                writer.write_all(b"Response from server").await.unwrap();
                println!("Response sent to {:?}", addr);
            });
        }

    }

    #[tokio::test]
    async fn test_instance() {

        task::spawn_blocking(|| {
            let instance = Tor::new().flag(TorFlag::DataDirectory("../tmp/tor-rust".into()))
            .flag(TorFlag::SocksPort(19050))
            // .flag(TorFlag::HiddenServiceDir("../tmp/tor-rust/maker/hs-dir".into()))
            // .flag(TorFlag::HiddenServiceVersion(HiddenServiceVersion::V3))
            // .flag(TorFlag::HiddenServicePort(
            //     TorAddress::Port(6102),
            //     None.into(),
            // ))
            // .flag(TorFlag::HiddenServiceDir("../tmp/tor-rust/taker/hs-dir".into()))
            // .flag(TorFlag::HiddenServiceVersion(HiddenServiceVersion::V3))
            // .flag(TorFlag::HiddenServicePort(
            //     TorAddress::Port(26102),
            //     None.into(),
            // ))
            .flag(TorFlag::HiddenServiceDir("../tmp/tor-rust/directory/hs-dir".into()))
            .flag(TorFlag::HiddenServiceVersion(HiddenServiceVersion::V3))
            .flag(TorFlag::HiddenServicePort(
                TorAddress::Port(8000),
                None.into(),
            ))
            .start();
            println!("Here in test {:?}",instance);
        });

        thread::sleep(Duration::from_millis(10000));

        tokio::spawn(async {
            run_server().await.unwrap();
        });
       

        // thread::sleep(Duration::from_millis(10000));

        // let value = post_maker_address_to_directory_servers(Network::Bitcoin,"edpxvwpwvgx7cijfruif65vorc32sc6lqn2k6bfl6czcyzgadmjnmsid.onion:6102").await;
        // println!("{:?}",value);
        
    }
}

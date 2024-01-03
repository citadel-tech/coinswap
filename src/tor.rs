use libtor::{HiddenServiceVersion,Tor,TorAddress,TorFlag};

const SOCKS_PORT: u16 = 19_050;
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
        Err(e) => eprintln!("tor error: {}", e),
    };
}

#[cfg(test)]
mod test {
    use std::{thread, time::Duration};

    use tokio::task;
    use crate::tor::*;

    #[tokio::test]
    async fn test_instance() {
        task::spawn_blocking(|| tor_instance());
        
        let proxy = reqwest::Proxy::all(format!("socks5://127.0.0.1:{}", SOCKS_PORT)).expect("Where is proxy");
        let client = reqwest::Client::builder().proxy(proxy).build().expect("Client should we build");

        thread::sleep(Duration::from_millis(2500));
        println!("---------------------------------request--------------------------------------------");
        let response = client
        .get(CHECK_URL.to_string())
        .send()
        .await;
        match response {
            Ok(r) => {
                let html = r.text().await.unwrap();
                println!("{:?}",html);
                match html.find("Congratulations") {
                    Some(_) => {
                        println!("Tor is online!");
                    }
                    None => {
                        println!("Received a response but not through Tor...");
                    }
                }
            }
            Err(e) => {
                println!("error: {}", e.to_string());
            }
        };
    
    }
}
use libtor::{HiddenServiceVersion,Tor,TorAddress,TorFlag};

const SOCKS_PORT: u16 = 19_050;
const CHECK_URL: &str = "https://check.torproject.org";

fn tor_instance() {
    match Tor::new().flag(TorFlag::DataDirectory("/temp/tor-rust".into())).flag(TorFlag::SocksPort(SOCKS_PORT)).flag(TorFlag::HiddenServiceDir("/tmp/tor-rust/hs-dir".into())).flag(TorFlag::HiddenServiceVersion(HiddenServiceVersion::V3)).flag(TorFlag::HiddenServicePort(TorAddress::Port(8000), None.into())).start() {
        Ok(r)=> println!("Here success: {:?}",r),
        Err(e) => println!("Here Error:{:?}",e)
    };
}

#[cfg(test)]
mod test {

    use crate::tor::*;

    #[test]
    fn test_instance() {
        tor_instance();
    }
}
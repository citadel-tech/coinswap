use coinswap_derive::ConfigBuilder;

#[derive(ConfigBuilder)]
pub struct Config {
    pub rpc_port: u16,
    pub min_swap_amount: u64,
    pub network_port: u16,
    pub control_port: u16,
    pub socks_port: u16,
    pub tor_auth_password: String,
    pub directory_server_address: Option<String>,
}

fn main() {
    let config = Config::builder()
        .rpc_port(6103)
        .min_swap_amount(10_000)
        .network_port(6102)
        .control_port(9051)
        .socks_port(9050)
        .tor_auth_password("password".to_string())
        .build()
        .unwrap();
    assert!(config.directory_server_address.is_none());

    let config = Config::builder()
        .rpc_port(6103)
        .min_swap_amount(10_000)
        .network_port(6102)
        .control_port(9051)
        .socks_port(9050)
        .tor_auth_password("password".to_string())
        .directory_server_address(
            "kizqnaslcb2r3mbk2vm77bdff3madcvddntmaaz2htmkyuw7sgh4ddqd.onion:8080".to_string(),
        )
        .build()
        .unwrap();
    assert!(config.directory_server_address.is_some());
}

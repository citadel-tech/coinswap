use clap::Parser;

/// wrapper binary to run the tor hidden service.
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct App {
    /// the network port
    #[clap(long, short = 'p')]
    port: u16,
    /// the socks port
    #[clap(long, short = 's')]
    socks_port: u16,
    /// the base directory for hidden service
    #[clap(long, short = 'd')]
    base_dir: String,
}

#[cfg(feature = "tor")]
fn main() -> Result<(), libtor::Error> {
    let args = App::parse();
    coinswap::tor::start_tor(args.socks_port, args.port, args.base_dir)
}

#[cfg(not(feature = "tor"))]
fn main() {
    println!("Error: tor feature is needed to run this binary.");
}

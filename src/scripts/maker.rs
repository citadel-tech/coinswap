use crate::{
    maker::{start_maker_server, Maker, MakerBehavior},
    wallet::RPCConfig,
};
use std::{
    path::PathBuf,
    sync::{Arc, RwLock},
    thread,
    time::Duration,
};

use crate::{error::TeleportError, wallet::WalletMode};

#[tokio::main]
pub async fn run_maker(
    wallet_file_name: &PathBuf,
    port: u16,
    wallet_mode: Option<WalletMode>,
    maker_behavior: MakerBehavior,
    kill_flag: Arc<RwLock<bool>>,
) -> Result<Arc<Maker>, TeleportError> {
    // Hardcoded for now, tor doesn't work yet.
    let onion_addrs = "myhiddenserviceaddress.onion:6102".to_string();
    let maker = Maker::init(
        wallet_file_name,
        &RPCConfig::default(),
        port,
        onion_addrs,
        wallet_mode,
        maker_behavior,
    )?;

    let arc_maker = Arc::new(maker);

    let maker_shut = arc_maker.clone();

    thread::spawn(move || {
        log::info!("Shutdown thread spawned");
        loop {
            thread::sleep(Duration::from_secs(3));
            if *kill_flag.read().unwrap() {
                maker_shut.shutdown().unwrap();
                break;
            }
        }
    });

    log::info!("Maker server starting");
    start_maker_server(arc_maker.clone()).await?;

    Ok(arc_maker)
}

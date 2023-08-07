use std::{
    convert::TryFrom,
    path::PathBuf,
    sync::{Arc, RwLock},
};

use bitcoincore_rpc::Client;

use crate::maker::server::{start_maker, MakerBehavior, MakerConfig};

use crate::{
    error::TeleportError,
    wallet::{RPCConfig, Wallet, WalletMode},
};

pub fn run_maker(
    wallet_file_name: &PathBuf,
    port: u16,
    wallet_mode: Option<WalletMode>,
    maker_behavior: MakerBehavior,
    kill_flag: Option<Arc<RwLock<bool>>>,
) -> Result<(), TeleportError> {
    let rpc_config = RPCConfig::default();

    let rpc = Client::try_from(&rpc_config)?;

    let mut wallet = Wallet::load(&rpc_config, wallet_file_name, wallet_mode)?;

    wallet.sync()?;

    let rpc_ptr = Arc::new(rpc);
    let wallet_ptr = Arc::new(RwLock::new(wallet));
    let config = MakerConfig {
        port,
        rpc_ping_interval_secs: 60,
        watchtower_ping_interval_secs: 300,
        directory_servers_refresh_interval_secs: 60 * 60 * 12, //12 hours
        maker_behavior,
        kill_flag: if kill_flag.is_none() {
            Arc::new(RwLock::new(false))
        } else {
            kill_flag.unwrap().clone()
        },
        idle_connection_timeout: 300,
    };
    start_maker(rpc_ptr, wallet_ptr, config);

    Ok(())
}

use std::{
    convert::TryFrom,
    path::PathBuf,
    sync::{Arc, RwLock},
};

use bitcoincore_rpc::Client;

use crate::{error::TeleportError, wallet::RPCConfig, watchtower::routines::start_watchtower};

pub fn run_watchtower(
    data_file_path: &PathBuf,
    kill_flag: Option<Arc<RwLock<bool>>>,
) -> Result<(), TeleportError> {
    let rpc_config = RPCConfig::default();
    let rpc = Client::try_from(&rpc_config)?;

    start_watchtower(
        &rpc,
        data_file_path,
        rpc_config.network,
        if kill_flag.is_none() {
            Arc::new(RwLock::new(false))
        } else {
            kill_flag.unwrap().clone()
        },
    );

    Ok(())
}

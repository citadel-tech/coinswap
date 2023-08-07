mod config;
pub mod offers;
mod routines;
mod taker;

use std::path::PathBuf;

use crate::{
    error::TeleportError,
    wallet::{RPCConfig, WalletMode},
};

pub use taker::{SwapParams, Taker};

pub use config::TakerConfig;

#[tokio::main]
pub async fn start_taker(
    rpc_config: Option<RPCConfig>,
    wallet_file: &PathBuf,
    wallet_mode: Option<WalletMode>,
    swap_params: SwapParams,
) {
    match run(rpc_config, wallet_file, wallet_mode, swap_params).await {
        Ok(_o) => (),
        Err(e) => log::error!("err {:?}", e),
    };
}

/// The main driver initializing and starting a swap round.
async fn run(
    rpc_config: Option<RPCConfig>,
    wallet_file: &PathBuf,
    wallet_mode: Option<WalletMode>,
    swap_params: SwapParams,
) -> Result<(), TeleportError> {
    let mut taker = Taker::init(wallet_file, rpc_config, wallet_mode).await?;
    taker.send_coinswap(swap_params).await?;
    Ok(())
}

use std::path::PathBuf;

use crate::{
    taker,
    taker::SwapParams,
    wallet::{RPCConfig, WalletMode},
};

use crate::taker::TakerBehavior;

pub fn run_taker(
    wallet_file: &PathBuf,
    wallet_mode: Option<WalletMode>,
    rpc_config: Option<RPCConfig>,
    fee_rate: u64,
    send_amount: u64,
    maker_count: u16,
    tx_count: u32,
    behavior: TakerBehavior,
) {
    let swap_params = SwapParams {
        send_amount,
        maker_count,
        tx_count,
        required_confirms: 1, // TODO: make it input params.
        fee_rate,
    };
    taker::start_taker(rpc_config, wallet_file, wallet_mode, swap_params, behavior)
}

//! Fee Estimation Module

use std::collections::HashMap;

use bitcoind::bitcoincore_rpc::{json::EstimateMode, RpcApi};
use serde::Deserialize;

use crate::{error::FeeEstimatorError, wallet::Wallet};

const MEMPOOL_FEES_API_URL: &str = "https://mempool.space/api/v1/fees/recommended";
const ESPLORA_FEES_API_URL: &str = "https://blockstream.info/api/fee-estimates";

/// Block Target for Fee Estimation
#[derive(Debug, Eq, PartialEq, Hash)]
pub enum BlockTarget {
    /// Next Block (high priority)
    Fastest,
    /// Within 6 Blocks
    Standard,
    /// Within 24 Blocks
    Economy,
}

/// Used to fetch fee rates from different public sources like mempool, esplora, electrum
pub struct FeeEstimator {
    wallet: Option<Wallet>, // Optional Wallet for estimatesmartfee RPC call
}

impl FeeEstimator {
    /// Create a new FeeEstimator with optional wallet for Bitcoin Core estimates
    pub fn new(wallet: Option<Wallet>) -> Self {
        Self { wallet }
    }

    /// Get fee rate for next block (highest priority)
    pub fn get_high_priority_rate(&self) -> Result<f64, FeeEstimatorError> {
        self.get_fee_rate(BlockTarget::Fastest)
    }

    /// Get fee rate for ~6 blocks (standard priority)
    pub fn get_mid_priority_rate(&self) -> Result<f64, FeeEstimatorError> {
        self.get_fee_rate(BlockTarget::Standard)
    }

    /// Get fee rate for ~24 blocks (economy priority)
    pub fn get_low_priority_rate(&self) -> Result<f64, FeeEstimatorError> {
        self.get_fee_rate(BlockTarget::Economy)
    }

    /// Get a specific fee rate with defaults (non-conservative, no multiplier)
    pub fn get_fee_rate(&self, target: BlockTarget) -> Result<f64, FeeEstimatorError> {
        let fees = self.estimate_fees()?;
        fees.get(&target).copied().ok_or_else(|| {
            FeeEstimatorError::MissingData(format!("Fee not available for target: {target:?}"))
        })
    }

    fn estimate_fees(&self) -> Result<HashMap<BlockTarget, f64>, FeeEstimatorError> {
        let mut combined_fees: HashMap<BlockTarget, Vec<f64>> = HashMap::new();
        combined_fees.insert(BlockTarget::Fastest, Vec::new());
        combined_fees.insert(BlockTarget::Standard, Vec::new());
        combined_fees.insert(BlockTarget::Economy, Vec::new());

        let results = std::thread::scope(|s| {
            let mut handles = Vec::new();

            if self.wallet.is_some() {
                let handle = s.spawn(|| self.estimate_smart_fee());
                handles.push(("Bitcoin Core", handle));
            }

            let mempool_handle = s.spawn(Self::fetch_mempool_fees);
            handles.push(("Mempool Space", mempool_handle));

            let esplora_handle = s.spawn(Self::fetch_esplora_fees);
            handles.push(("Esplora", esplora_handle));

            let mut results = Vec::new();
            for (source, handle) in handles {
                results.push((source, handle.join()));
            }
            results
        });

        for (source, result) in results {
            let fees = result.map_err(|_| FeeEstimatorError::ThreadError)?;
            if let Ok(fees) = fees {
                for (target, fee) in fees {
                    log::debug!("{source} Fee Estimates {target:?}: {fee} sat/vB",);
                    if let Some(target_fees) = combined_fees.get_mut(&target) {
                        target_fees.push(fee);
                    }
                }
            }
        }

        // Combined fees using simple mean
        let mut final_fees = HashMap::new();

        for (target, fees) in combined_fees {
            if !fees.is_empty() {
                let avg = fees.iter().sum::<f64>() / fees.len() as f64;
                log::info!("Fee Estimates {target:?}: {avg} sat/vB");
                final_fees.insert(target, avg);
            }
        }

        if final_fees.is_empty() {
            return Err(FeeEstimatorError::NoFeeSources);
        }

        Ok(final_fees)
    }

    /// Fetches live feerates for different blocktargets from mempool.space
    pub fn fetch_mempool_fees() -> Result<HashMap<BlockTarget, f64>, FeeEstimatorError> {
        let response = minreq::get(MEMPOOL_FEES_API_URL)
            .send()?
            .json::<MempoolFeeResponse>()?;

        let mut fees = HashMap::new();

        fees.insert(BlockTarget::Fastest, response.fastest_fee);
        fees.insert(BlockTarget::Standard, response.hour_fee);
        fees.insert(BlockTarget::Economy, response.economy_fee);

        Ok(fees)
    }

    fn estimate_smart_fee(&self) -> Result<HashMap<BlockTarget, f64>, FeeEstimatorError> {
        let wallet = self.wallet.as_ref().ok_or(FeeEstimatorError::NoWallet)?;
        let mut fees = HashMap::new();
        let targets = [1, 6, 24]; // Same as Block Targets
        for target in targets {
            let res = wallet
                .rpc
                .estimate_smart_fee(target, Some(EstimateMode::Conservative))?;
            if let Some(fee_rate) = res.fee_rate {
                let fee_rate_sats_vbyte = fee_rate.to_sat() as f64 / 1000.0;
                match target {
                    1 => {
                        fees.insert(BlockTarget::Fastest, fee_rate_sats_vbyte);
                    }
                    6 => {
                        fees.insert(BlockTarget::Standard, fee_rate_sats_vbyte);
                    }
                    24 => {
                        fees.insert(BlockTarget::Economy, fee_rate_sats_vbyte);
                    }
                    _ => (),
                }
            } else {
                return Err(FeeEstimatorError::MissingData(format!(
                    "No fee rate returned for {target} blocks"
                )));
            }
        }
        Ok(fees)
    }

    /// Fetches (live + historical averages) feerates for different blocktargets from blockstream
    pub fn fetch_esplora_fees() -> Result<HashMap<BlockTarget, f64>, FeeEstimatorError> {
        let response = minreq::get(ESPLORA_FEES_API_URL)
            .send()?
            .json::<EsploraFeeResponse>()?
            .fees;

        let mut fees = HashMap::new();

        let fee_1 = response
            .get("1")
            .ok_or_else(|| {
                FeeEstimatorError::MissingData(
                    "No fee estimation for 1 block in Esplora response".to_string(),
                )
            })?
            .to_owned();

        let fee_6 = response
            .get("6")
            .ok_or_else(|| {
                FeeEstimatorError::MissingData(
                    "No fee estimation for 6 blocks in Esplora response".to_string(),
                )
            })?
            .to_owned();

        let fee_24 = response
            .get("24")
            .ok_or_else(|| {
                FeeEstimatorError::MissingData(
                    "No fee estimation for 24 blocks in Esplora response".to_string(),
                )
            })?
            .to_owned();

        fees.insert(BlockTarget::Fastest, fee_1);
        fees.insert(BlockTarget::Standard, fee_6);
        fees.insert(BlockTarget::Economy, fee_24);

        Ok(fees)
    }
}

#[derive(Debug, Deserialize)]
struct MempoolFeeResponse {
    #[serde(rename = "fastestFee")]
    fastest_fee: f64,
    #[serde(rename = "hourFee")]
    hour_fee: f64,
    #[serde(rename = "economyFee")]
    economy_fee: f64,
}

#[derive(Debug, Deserialize)]
struct EsploraFeeResponse {
    #[serde(flatten)]
    fees: HashMap<String, f64>,
}

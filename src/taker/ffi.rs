//! FFI-compatible types for the Taker module.
//!
//! This module provides Foreign Function Interface (FFI) compatible data structures
//! for exposing swap functionality and reporting to other languages. These types are
//! designed to be used by the FFI layer(uniffi, napi) and provide simplified,
//! language-agnostic representations of the core swap data structures.
//!
//! - [`MakerFeeInfo`]: Detailed fee breakdown for individual makers in a swap route
//! - [`SwapReport`]: Comprehensive report of a completed swap transaction
//!
//! These structures use primitive types and standard collections (Vec, String) that
//! can be easily marshaled across FFI boundaries, avoiding Rust-specific types that
//! would be difficult to represent in other languages.

/// Information about individual maker fees in a swap
#[derive(Debug)]
pub struct MakerFeeInfo {
    /// Index of maker in the swap route
    pub maker_index: usize,
    /// Maker Addresses (Onion:Port)
    pub maker_address: String,
    /// The fixed Base Fee for each maker
    pub base_fee: f64,
    /// Dynamic Amount Fee for each maker
    pub amount_relative_fee: f64,
    /// Dynamic Time Fee(Decreases for subsequent makers) for each maker
    pub time_relative_fee: f64,
    /// All inclusive fee for each maker
    pub total_fee: f64,
}

/// Complete swap report containing all swap information
#[derive(Debug)]
pub struct SwapReport {
    /// Unique swap ID
    pub swap_id: String,
    /// Duration of the swap in seconds
    pub swap_duration_seconds: f64,
    /// Target amount for the swap
    pub target_amount: u64,
    /// Total input amount
    pub total_input_amount: u64,
    /// Total output amount
    pub total_output_amount: u64,
    /// Number of makers involved
    pub makers_count: usize,
    /// List of maker addresses used
    pub maker_addresses: Vec<String>,
    /// Total number of funding transactions
    pub total_funding_txs: usize,
    /// Funding transaction IDs organized by hops
    pub funding_txids_by_hop: Vec<Vec<String>>,
    /// Total fees paid
    pub total_fee: u64,
    /// Total maker fees
    pub total_maker_fees: u64,
    /// Mining fees
    pub mining_fee: u64,
    /// Fee percentage relative to target amount
    pub fee_percentage: f64,
    /// Individual maker fee information
    pub maker_fee_info: Vec<MakerFeeInfo>,
    /// Input UTXOs amounts
    pub input_utxos: Vec<u64>,
    /// Output change UTXOs amounts
    pub output_change_amounts: Vec<u64>,
    /// Output swap coin UTXOs amounts
    pub output_swap_amounts: Vec<u64>,
    /// Output change coin UTXOs with amounts and addresses (amount, address)
    pub output_change_utxos: Vec<(u64, String)>,
    /// Output swap coin UTXOs with amounts and addresses (amount, address)
    pub output_swap_utxos: Vec<(u64, String)>,
}

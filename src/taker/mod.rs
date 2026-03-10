//! Defines a Coinswap Taker Client.
//!
//! This also contains the entire swap workflow as major decision makings are involved for the Taker. Makers are
//! simple request-response servers. The Taker handles all the necessary communications between one or many makers to route the swap across various makers. Description of
//! protocol workflow is described in the [protocol between takers and makers](https://github.com/citadel-tech/Coinswap-Protocol-Specification/blob/main/v1/3_protocol-flow.md)

mod config;
pub mod error;
pub mod offers;

// Unified API
mod background_services;
mod legacy_swap;
mod legacy_verification;
pub mod swap_tracker;
mod taproot_swap;
mod taproot_verification;
pub mod unified_api;

pub use config::TakerConfig;

pub use offers::{format_state, MakerOfferCandidate, MakerState, OfferBook};
#[cfg(feature = "integration-test")]
pub use unified_api::UnifiedTakerBehavior;
pub use unified_api::{
    MakerFeeInfo, SwapSummary, UnifiedSwapParams, UnifiedSwapReport, UnifiedTaker,
    UnifiedTakerConfig,
};

//! Contains Taker-related behaviors most protocol logic.
//!
//! The Taker handles all the necessary communications between one or many makers to route the swap across various makers. Implementation of
//! coinswap Taker protocol described in the [protocol between takers and makers](https://github.com/utxo-teleport/teleport-transactions#protocol-between-takers-and-makers)

mod api;
mod config;
pub mod error;
pub mod offers;
mod routines;

pub use self::api::TakerBehavior;
pub use api::{SwapParams, Taker};
pub use config::TakerConfig;

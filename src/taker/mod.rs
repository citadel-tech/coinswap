//! Defines a Coinswap Taker Client.
//!
//! This module contains the entire swap workflow as major decision makings are involved for the Taker. Makers are simple request-response servers.
//! The Taker handles all necessary communications between one or many makers to route the swap:
//! - Manages multi-maker communication
//! - Routes swaps through maker network
//! - Controls swap protocol workflow
//! - Makes critical swap decisions
//!
//! For detailed protocol documentation between takers and makers, see the
//! [dev book](https://github.com/citadel-tech/coinswap/blob/master/docs/dev-book.md).
mod api;
mod config;
pub mod error;
pub mod offers;
mod routines;

pub use self::api::TakerBehavior;
pub use api::{SwapParams, Taker};
pub use config::TakerConfig;

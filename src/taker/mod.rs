//! Defines a Coinswap Taker Client.
//!
//! This also contains the entire swap workflow as major decision makings are involved for the Taker. Makers are
//! simple request-response servers. The Taker handles all the necessary communications between one or many makers to route the swap across various makers. Description of
//! protocol workflow is described in the [protocol between takers and makers](https://github.com/citadel-tech/coinswap/blob/master/docs/dev-book.md)

mod api;
mod config;
pub mod error;
pub mod offers;
mod routines;

pub use self::api::TakerBehavior;
pub use api::{SwapParams, Taker};
pub use config::TakerConfig;

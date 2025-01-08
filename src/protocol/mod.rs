//! Defines the Contract Transaction and Protocol Messages.

pub(crate) mod contract;
pub mod error;
pub mod messages;

pub(crate) use contract::Hash160;

pub use messages::{DnsMetadata, DnsRequest};

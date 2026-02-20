//! Defines the Contract Transaction and Protocol Messages.

pub mod common_messages;
pub mod legacy_messages;
pub mod router;
pub mod taproot_messages;

pub(crate) mod contract;
pub mod contract2;
pub mod error;
pub mod musig2;
pub mod musig_interface;

pub mod messages;
pub mod messages2;

pub use common_messages::ProtocolVersion;
pub(crate) use contract::Hash160;

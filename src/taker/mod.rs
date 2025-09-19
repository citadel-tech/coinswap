//! Defines a Coinswap Taker Client.
//!
//! This also contains the entire swap workflow as major decision makings are involved for the Taker. Makers are
//! simple request-response servers. The Taker handles all the necessary communications between one or many makers to route the swap across various makers. Description of
//! protocol workflow is described in the [protocol between takers and makers](https://github.com/citadel-tech/Coinswap-Protocol-Specification/blob/main/v1/3_protocol-flow.md)

pub mod api;
mod config;
pub mod error;
pub(crate) mod offers;
mod routines;
use std::io::Write;

use std::{io::BufWriter, net::TcpStream};

pub use self::api::TakerBehavior;
pub use api::{SwapParams, Taker};
pub use config::TakerConfig;
use error::TakerError;

/// This method adds a prefix for
/// maker to identify if its
/// taker or not
pub fn send_message_with_prefix(
    socket_writer: &mut TcpStream,
    message: &impl serde::Serialize,
) -> Result<(), TakerError> {
    let mut writer = BufWriter::new(socket_writer);
    let mut msg_bytes = Vec::new();
    msg_bytes.push(0x01);
    msg_bytes.extend(serde_cbor::to_vec(message)?);
    let msg_len = (msg_bytes.len() as u32).to_be_bytes();
    let mut to_send = Vec::with_capacity(msg_bytes.len() + msg_len.len());
    to_send.extend(msg_len);
    to_send.extend(msg_bytes);
    writer.write_all(&to_send)?;
    writer.flush()?;
    Ok(())
}

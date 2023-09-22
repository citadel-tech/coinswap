#![doc = include_str!("../README.md")]

extern crate bitcoin;
extern crate bitcoind;

pub mod error;
pub mod maker;
pub mod market;
pub mod protocol;
pub mod scripts;
pub mod taker;
#[cfg(feature = "integration-test")]
pub mod test_framework;
pub mod utill;
pub mod wallet;
// Diasable watchtower for now. Handle contract watching
// individually for maker and Taker.
//pub mod watchtower;

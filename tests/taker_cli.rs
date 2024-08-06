#![cfg(feature = "integration-test")]

use std::assert_eq;

use std::{convert::TryFrom, fs, path::PathBuf, process::Command, str::FromStr};

mod test_framework;
use bitcoin::Amount;
use bitcoind::bitcoincore_rpc::{Auth, Client, RpcApi};
use coinswap::{
    maker::{start_maker_server, MakerBehavior},
    taker::SwapParams,
    utill::ConnectionType,
    wallet::{Destination, RPCConfig, SendAmount, Wallet},
};

use test_framework::*;

#[tokio::test]
async fn test_taker_send_to_address() {
    // ---- Setup ----
    let makers_config_map = [
        ((6102, None), MakerBehavior::Normal),
        ((16102, None), MakerBehavior::Normal),
    ];

    // Initiate test framework, Makers and a Taker with default behavior.
    let (test_framework, taker, makers, directory_server_instance) = TestFramework::init(
        None,
        makers_config_map.into(),
        None,
        ConnectionType::CLEARNET,
    )
    .await;

    let taker_address = taker
        .write()
        .unwrap()
        .get_wallet_mut()
        .get_next_external_address()
        .expect("Failed to generate Taker address");

    // let maker_address = makers.get(0).unwrap()
    //     .generate_new_address()
    //     .await
    //     .expect("Failed to generate Maker address");

    // Define the amount to send
    let amount = 0.005;

    // Run the send_to_address command
    let output = Command::new("cargo")
        .arg("run")
        .arg("--bin")
        .arg("taker")
        .arg("send-to-address")
        .arg(taker_address.to_string())
        .arg(amount.to_string())
        .output()
        .expect("Failed to execute command");

    // ---- Assertions ----

    // Check the command output
    assert!(output.status.success(), "Command execution failed");

    // Verify the transaction
    // let balance = client
    //     .get_balance(None, None)
    //     .expect("Failed to get balance");
    // assert_eq!(balance, amount, "Balance does not match the sent amount");
}

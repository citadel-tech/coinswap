//! RPC server for taproot coinswap maker
//!
//! This module provides RPC functionality for the taproot maker server.
//! It handles RPC requests from the maker-cli tool and provides an interface
//! for managing the taproot maker remotely.

use std::{
    io::ErrorKind,
    net::{TcpListener, TcpStream},
    sync::{atomic::Ordering::Relaxed, Arc},
    thread::sleep,
    time::Duration,
};

use bitcoin::{Address, Amount};

use crate::{
    maker::{api2::Maker, error::MakerError, RpcMsgReq, RpcMsgResp},
    utill::{get_tor_hostname, read_message, send_message, HEART_BEAT_INTERVAL},
    wallet::Destination,
};
use std::str::FromStr;

fn handle_request_taproot(maker: &Arc<Maker>, socket: &mut TcpStream) -> Result<(), MakerError> {
    let msg_bytes = read_message(socket)?;
    let rpc_request: RpcMsgReq = serde_cbor::from_slice(&msg_bytes)?;
    log::info!("Taproot RPC request received: {rpc_request:?}");

    let resp = match rpc_request {
        RpcMsgReq::Ping => RpcMsgResp::Pong,
        RpcMsgReq::ContractUtxo => {
            let utxos = maker
                .get_wallet()
                .read()?
                .list_live_timelock_contract_spend_info()?
                .iter()
                .map(|data| crate::utill::UTXO::from_utxo_data(data.clone()))
                .collect::<Vec<_>>();
            RpcMsgResp::ContractUtxoResp { utxos }
        }
        RpcMsgReq::FidelityUtxo => {
            let utxos = maker
                .get_wallet()
                .read()?
                .list_fidelity_spend_info()?
                .iter()
                .map(|data| crate::utill::UTXO::from_utxo_data(data.clone()))
                .collect::<Vec<_>>();
            RpcMsgResp::FidelityUtxoResp { utxos }
        }
        RpcMsgReq::Utxo => {
            let utxos = maker
                .get_wallet()
                .read()?
                .list_all_utxo_spend_info()?
                .iter()
                .map(|data| crate::utill::UTXO::from_utxo_data(data.clone()))
                .collect::<Vec<_>>();
            RpcMsgResp::UtxoResp { utxos }
        }
        RpcMsgReq::SwapUtxo => {
            let utxos = maker
                .get_wallet()
                .read()?
                .list_incoming_swap_coin_utxo_spend_info()?
                .iter()
                .map(|data| crate::utill::UTXO::from_utxo_data(data.clone()))
                .collect::<Vec<_>>();
            RpcMsgResp::SwapUtxoResp { utxos }
        }
        RpcMsgReq::Balances => {
            let balances = maker.get_wallet().read()?.get_balances()?;
            RpcMsgResp::TotalBalanceResp(balances)
        }
        RpcMsgReq::NewAddress => {
            let new_address = maker.get_wallet().write()?.get_next_external_address()?;
            RpcMsgResp::NewAddressResp(new_address.to_string())
        }
        RpcMsgReq::SendToAddress {
            address,
            amount,
            feerate,
        } => {
            let amount = Amount::from_sat(amount);
            let destination = Destination::Multi {
                outputs: vec![(
                    Address::from_str(&address).unwrap().assume_checked(),
                    amount,
                )],
                op_return_data: None,
            };

            let coins_to_send = maker
                .get_wallet()
                .read()?
                .coin_select(amount, feerate, None)?;
            let tx = maker.get_wallet().write()?.spend_from_wallet(
                feerate,
                destination,
                &coins_to_send,
            )?;

            let txid = maker.get_wallet().read()?.send_tx(&tx)?;

            maker.get_wallet().write()?.sync_no_fail();

            RpcMsgResp::SendToAddressResp(txid.to_string())
        }
        RpcMsgReq::GetDataDir => RpcMsgResp::GetDataDirResp(maker.get_data_dir().to_path_buf()),
        RpcMsgReq::GetTorAddress => {
            if cfg!(feature = "integration-test") {
                RpcMsgResp::GetTorAddressResp("Maker is not running on TOR".to_string())
            } else {
                let hostname = get_tor_hostname(
                    maker.get_data_dir(),
                    maker.config.control_port,
                    maker.config.network_port,
                    &maker.config.tor_auth_password,
                )?;
                let address = format!("{}:{}", hostname, maker.config.network_port);
                RpcMsgResp::GetTorAddressResp(address)
            }
        }
        RpcMsgReq::Stop => {
            maker.shutdown.store(true, Relaxed);
            RpcMsgResp::Shutdown
        }
        RpcMsgReq::ListFidelity => {
            let list = maker.get_wallet().read()?.display_fidelity_bonds()?;
            RpcMsgResp::ListBonds(list)
        }
        RpcMsgReq::SyncWallet => {
            log::info!("Initializing taproot wallet sync");
            if let Err(e) = maker.get_wallet().write()?.sync() {
                RpcMsgResp::ServerError(format!("{e:?}"))
            } else {
                log::info!("Completed taproot wallet sync");
                RpcMsgResp::Pong
            }
        }
    };

    if let Err(e) = send_message(socket, &resp) {
        log::error!("Error sending taproot RPC response {e:?}");
    }

    Ok(())
}

pub(crate) fn start_rpc_server_taproot(maker: Arc<Maker>) -> Result<(), MakerError> {
    let rpc_port = maker.config.rpc_port;
    let rpc_socket = format!("127.0.0.1:{rpc_port}");
    let listener = Arc::new(TcpListener::bind(&rpc_socket)?);
    log::info!(
        "[{}] Taproot RPC socket binding successful at {}",
        maker.config.network_port,
        rpc_socket
    );

    listener.set_nonblocking(true)?;

    while !maker.shutdown.load(Relaxed) {
        match listener.accept() {
            Ok((mut stream, addr)) => {
                log::info!("Got taproot RPC request from: {addr}");
                stream.set_read_timeout(Some(Duration::from_secs(20)))?;
                stream.set_write_timeout(Some(Duration::from_secs(20)))?;
                // Do not cause hard error if a rpc request fails
                if let Err(e) = handle_request_taproot(&maker, &mut stream) {
                    log::error!("Error processing taproot RPC Request: {e:?}");
                    // Send the error back to client.
                    if let Err(e) =
                        send_message(&mut stream, &RpcMsgResp::ServerError(format!("{e:?}")))
                    {
                        log::error!("Error sending taproot RPC response {e:?}");
                    };
                }
            }

            Err(e) => {
                if e.kind() == ErrorKind::WouldBlock {
                    // do nothing
                } else {
                    log::error!("Error accepting taproot RPC connection: {e:?}");
                }
            }
        }

        sleep(HEART_BEAT_INTERVAL);
    }

    Ok(())
}

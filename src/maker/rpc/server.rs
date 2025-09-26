use std::{
    io::ErrorKind,
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering::Relaxed},
        Arc,
    },
    thread::sleep,
    time::Duration,
};

use bitcoin::{Address, Amount};

use super::messages::RpcMsgReq;
use crate::{
    maker::{config::MakerConfig, error::MakerError, rpc::messages::RpcMsgResp},
    utill::{get_tor_hostname, read_message, send_message, HEART_BEAT_INTERVAL, UTXO},
    wallet::{Destination, Wallet},
};
use std::{path::Path, str::FromStr, sync::RwLock};

pub trait MakerRpc {
    fn wallet(&self) -> &RwLock<Wallet>;
    fn data_dir(&self) -> &Path;
    fn config(&self) -> &MakerConfig;
    fn shutdown(&self) -> &AtomicBool;
}

fn handle_request<M: MakerRpc>(maker: &Arc<M>, socket: &mut TcpStream) -> Result<(), MakerError> {
    let msg_bytes = read_message(socket)?;
    let rpc_request: RpcMsgReq = serde_cbor::from_slice(&msg_bytes)?;
    log::info!("RPC request received: {rpc_request:?}");

    let resp = match rpc_request {
        RpcMsgReq::Ping => RpcMsgResp::Pong,
        RpcMsgReq::ContractUtxo => {
            let utxos = maker
                .wallet()
                .read()?
                .list_live_timelock_contract_spend_info()
                .into_iter()
                .map(UTXO::from_utxo_data)
                .collect();
            RpcMsgResp::ContractUtxoResp { utxos }
        }
        RpcMsgReq::FidelityUtxo => {
            let utxos = maker
                .wallet()
                .read()?
                .list_fidelity_spend_info()
                .into_iter()
                .map(UTXO::from_utxo_data)
                .collect();
            RpcMsgResp::FidelityUtxoResp { utxos }
        }
        RpcMsgReq::Utxo => {
            let utxos = maker
                .wallet()
                .read()?
                .list_all_utxo_spend_info()
                .into_iter()
                .map(UTXO::from_utxo_data)
                .collect();
            RpcMsgResp::UtxoResp { utxos }
        }
        RpcMsgReq::SwapUtxo => {
            let utxos = maker
                .wallet()
                .read()?
                .list_incoming_swap_coin_utxo_spend_info()
                .into_iter()
                .map(UTXO::from_utxo_data)
                .collect();
            RpcMsgResp::SwapUtxoResp { utxos }
        }
        RpcMsgReq::Balances => {
            let balances = maker.wallet().read()?.get_balances()?;
            RpcMsgResp::TotalBalanceResp(balances)
        }
        RpcMsgReq::NewAddress => {
            let new_address = maker.wallet().write()?.get_next_external_address()?;
            RpcMsgResp::NewAddressResp(new_address.to_string())
        }
        RpcMsgReq::SendToAddress {
            address,
            amount,
            feerate,
        } => {
            let amount = Amount::from_sat(amount);
            let outputs = vec![(
                Address::from_str(&address).unwrap().assume_checked(),
                amount,
            )];
            let destination = Destination::Multi {
                outputs,
                op_return_data: None,
            };

            let coins_to_send = maker
                .wallet()
                .read()?
                .coin_select(amount, feerate, None)?;
            let tx = maker.wallet().write()?.spend_from_wallet(
                feerate,
                destination,
                &coins_to_send,
            )?;

            let txid = maker.wallet().read()?.send_tx(&tx)?;

            maker.wallet().write()?.sync_no_fail();

            RpcMsgResp::SendToAddressResp(txid.to_string())
        }
        RpcMsgReq::GetDataDir => RpcMsgResp::GetDataDirResp(maker.data_dir().to_path_buf()),
        RpcMsgReq::GetTorAddress => {
            if cfg!(feature = "integration-test") {
                RpcMsgResp::GetTorAddressResp("Maker is not running on TOR".to_string())
            } else {
                let hostname = get_tor_hostname(
                    maker.data_dir(),
                    maker.config().control_port,
                    maker.config().network_port,
                    &maker.config().tor_auth_password,
                )?;
                let address = format!("{}:{}", hostname, maker.config().network_port);
                RpcMsgResp::GetTorAddressResp(address)
            }
        }
        RpcMsgReq::Stop => {
            maker.shutdown().store(true, Relaxed);
            RpcMsgResp::Shutdown
        }

        RpcMsgReq::ListFidelity => {
            let list = maker.wallet().read()?.display_fidelity_bonds()?;
            RpcMsgResp::ListBonds(list)
        }
        RpcMsgReq::SyncWallet => {
            log::info!("Initializing wallet sync");
            let mut wallet = maker.wallet().write()?;
            if let Err(e) = wallet.sync() {
                RpcMsgResp::ServerError(format!("{e:?}"))
            } else {
                log::info!("Completed wallet sync");
                wallet.save_to_disk()?;
                RpcMsgResp::Pong
            }
        }
    };

    if let Err(e) = send_message(socket, &resp) {
        log::error!("Error sending RPC response {e:?}");
    }

    Ok(())
}

pub(crate) fn start_rpc_server<M: MakerRpc>(maker: Arc<M>) -> Result<(), MakerError> {
    let rpc_port = maker.config().rpc_port;
    let rpc_socket = format!("127.0.0.1:{rpc_port}");
    let listener = Arc::new(TcpListener::bind(&rpc_socket)?);
    log::info!(
        "[{}] RPC socket binding successful at {}",
        maker.config().network_port,
        rpc_socket
    );

    listener.set_nonblocking(true)?;

    while !maker.shutdown().load(Relaxed) {
        match listener.accept() {
            Ok((mut stream, addr)) => {
                log::info!("Got RPC request from: {addr}");
                stream.set_read_timeout(Some(Duration::from_secs(20)))?;
                stream.set_write_timeout(Some(Duration::from_secs(20)))?;
                // Do not cause hard error if a rpc request fails
                if let Err(e) = handle_request(&maker, &mut stream) {
                    log::error!("Error processing RPC Request: {e:?}");
                    // Send the error back to client.
                    if let Err(e) =
                        send_message(&mut stream, &RpcMsgResp::ServerError(format!("{e:?}")))
                    {
                        log::error!("Error sending RPC response {e:?}");
                    };
                }
            }

            Err(e) => {
                if e.kind() == ErrorKind::WouldBlock {
                    // do nothing
                } else {
                    log::error!("Error accepting RPC connection: {e:?}");
                }
            }
        }

        sleep(HEART_BEAT_INTERVAL);
    }

    Ok(())
}

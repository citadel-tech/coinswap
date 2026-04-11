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

use bitcoin::{Amount, Network};

use super::messages::RpcMsgReq;
use crate::{
    maker::{api::MakerServerConfig, error::MakerError, rpc::messages::RpcMsgResp},
    utill::{
        parse_checked_address, read_message, send_message, TorError, HEART_BEAT_INTERVAL, UTXO,
    },
    wallet::{AddressType, Destination, Wallet},
};
use std::{path::Path, sync::RwLock};

pub trait MakerRpc {
    fn wallet(&self) -> &RwLock<Wallet>;
    fn data_dir(&self) -> &Path;
    fn config(&self) -> &MakerServerConfig;
    fn shutdown(&self) -> &AtomicBool;
    fn get_tor_hostname(&self) -> Result<String, TorError>;
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
                .map(|(utxo, spend_info)| UTXO::from_utxo_data((utxo.clone(), spend_info.clone())))
                .collect();
            RpcMsgResp::ContractUtxoResp { utxos }
        }
        RpcMsgReq::FidelityUtxo => {
            let utxos = maker
                .wallet()
                .read()?
                .list_fidelity_spend_info()
                .into_iter()
                .map(|(utxo, spend_info)| UTXO::from_utxo_data((utxo.clone(), spend_info.clone())))
                .collect();
            RpcMsgResp::FidelityUtxoResp { utxos }
        }
        RpcMsgReq::Utxo => {
            let utxos = maker
                .wallet()
                .read()?
                .list_all_utxo_spend_info()
                .into_iter()
                .map(|(utxo, spend_info)| UTXO::from_utxo_data((utxo.clone(), spend_info.clone())))
                .collect();
            RpcMsgResp::UtxoResp { utxos }
        }
        RpcMsgReq::SwapUtxo => {
            let utxos = maker
                .wallet()
                .read()?
                .list_incoming_swap_coin_utxo_spend_info()
                .into_iter()
                .map(|(utxo, spend_info)| UTXO::from_utxo_data((utxo.clone(), spend_info.clone())))
                .collect();
            RpcMsgResp::SwapUtxoResp { utxos }
        }
        RpcMsgReq::Balances => {
            let balances = maker.wallet().read()?.get_balances()?;
            RpcMsgResp::TotalBalanceResp(balances)
        }
        RpcMsgReq::NewAddress => {
            let new_address = maker
                .wallet()
                .write()?
                .get_next_external_address(AddressType::P2TR)?;
            RpcMsgResp::NewAddressResp(new_address.to_string())
        }
        RpcMsgReq::SendToAddress {
            address,
            amount,
            feerate,
        } => {
            let amount = Amount::from_sat(amount);
            let destination_address =
                parse_rpc_destination_address(&address, maker.config().network)?;
            let outputs = vec![(destination_address, amount)];
            let destination = Destination::Multi {
                outputs,
                op_return_data: None,
                change_address_type: AddressType::P2TR,
            };

            let coins_to_send = maker
                .wallet()
                .read()?
                .coin_select(amount, feerate, None, None)?;
            let tx =
                maker
                    .wallet()
                    .write()?
                    .spend_from_wallet(feerate, destination, &coins_to_send)?;

            let txid = maker.wallet().read()?.send_tx(&tx)?;

            log::info!("Sync at:----handle_request----");
            maker.wallet().write()?.sync_and_save()?;

            RpcMsgResp::SendToAddressResp(txid.to_string())
        }
        RpcMsgReq::GetDataDir => RpcMsgResp::GetDataDirResp(maker.data_dir().to_path_buf()),
        RpcMsgReq::GetTorAddress => {
            if cfg!(feature = "integration-test") {
                RpcMsgResp::GetTorAddressResp("Maker is not running on TOR".to_string())
            } else {
                let hostname = maker.get_tor_hostname()?;
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
            if let Err(e) = wallet.sync_and_save() {
                RpcMsgResp::ServerError(e.to_string())
            } else {
                log::info!("Completed wallet sync");
                RpcMsgResp::Pong
            }
        }
    };

    if let Err(e) = send_message(socket, &resp) {
        log::error!("Error sending RPC response {e:?}");
    }

    Ok(())
}

fn parse_rpc_destination_address(
    address: &str,
    network: Network,
) -> Result<bitcoin::Address, MakerError> {
    parse_checked_address(address, network).map_err(MakerError::from)
}

pub(crate) fn start_rpc_server<M: MakerRpc>(maker: Arc<M>) -> Result<(), MakerError> {
    let rpc_port = maker.config().rpc_port;
    let listener = TcpListener::bind(("127.0.0.1", rpc_port))?;
    let rpc_socket = format!("127.0.0.1:{rpc_port}");
    let listener = Arc::new(listener);
    log::info!(
        "[{}] RPC socket binding successful at {}",
        rpc_port,
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

#[cfg(test)]
mod tests {
    use super::parse_rpc_destination_address;
    use crate::{error::NetError, maker::error::MakerError};
    use bitcoin::Network;

    #[test]
    fn parse_rpc_destination_address_rejects_invalid_address() {
        let err = parse_rpc_destination_address("not-an-address", Network::Regtest)
            .expect_err("invalid address should fail");
        assert!(matches!(
            err,
            MakerError::Net(NetError::InvalidNetworkAddressDetailed(_))
        ));
    }

    #[test]
    fn parse_rpc_destination_address_rejects_wrong_network() {
        let err =
            parse_rpc_destination_address("1BoatSLRHtKNngkdXEeobR76b53LETtpyT", Network::Regtest)
                .expect_err("mainnet address should fail for regtest");
        assert!(matches!(
            err,
            MakerError::Net(NetError::InvalidNetworkAddressDetailed(_))
        ));
    }
}

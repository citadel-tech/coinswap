use std::{
    io::ErrorKind,
    net::{TcpListener, TcpStream},
    sync::{atomic::Ordering::Relaxed, Arc},
    thread::sleep,
    time::Duration,
};

use bitcoin::{Address, Amount};

use super::messages::RpcMsgReq;
use crate::{
    maker::{error::MakerError, rpc::messages::RpcMsgResp, Maker},
    utill::{get_tor_hostname, read_message, send_message, ConnectionType, HEART_BEAT_INTERVAL},
    wallet::{Destination, SendAmount},
};
use std::str::FromStr;

fn handle_request(maker: &Arc<Maker>, socket: &mut TcpStream) -> Result<(), MakerError> {
    let msg_bytes = read_message(socket)?;
    let rpc_request: RpcMsgReq = serde_cbor::from_slice(&msg_bytes)?;
    log::info!("RPC request received: {:?}", rpc_request);

    let resp = match rpc_request {
        RpcMsgReq::Ping => RpcMsgResp::Pong,
        RpcMsgReq::ContractUtxo => {
            let utxos = maker
                .get_wallet()
                .read()?
                .list_live_contract_spend_info(None)?
                .iter()
                .map(|(l, _)| l.clone())
                .collect::<Vec<_>>();
            RpcMsgResp::ContractUtxoResp { utxos }
        }
        RpcMsgReq::FidelityUtxo => {
            let utxos = maker
                .get_wallet()
                .read()?
                .list_fidelity_spend_info(None)?
                .iter()
                .map(|(l, _)| l.clone())
                .collect::<Vec<_>>();
            RpcMsgResp::FidelityUtxoResp { utxos }
        }
        RpcMsgReq::Utxo => {
            let utxos = maker
                .get_wallet()
                .read()?
                .list_all_utxo_spend_info(None)?
                .iter()
                .map(|(l, _)| l.clone())
                .collect::<Vec<_>>();
            RpcMsgResp::UtxoResp { utxos }
        }
        RpcMsgReq::SwapUtxo => {
            let utxos = maker
                .get_wallet()
                .read()?
                .list_swap_coin_utxo_spend_info(None)?
                .iter()
                .map(|(l, _)| l.clone())
                .collect::<Vec<_>>();
            RpcMsgResp::SwapUtxoResp { utxos }
        }
        RpcMsgReq::ContractBalance => {
            let balance = maker.get_wallet().read()?.balance_live_contract(None)?;
            RpcMsgResp::ContractBalanceResp(balance.to_sat())
        }
        RpcMsgReq::FidelityBalance => {
            let balance = maker.get_wallet().read()?.balance_fidelity_bonds(None)?;
            RpcMsgResp::FidelityBalanceResp(balance.to_sat())
        }
        RpcMsgReq::Balance => {
            let balance = maker.get_wallet().read()?.spendable_balance()?;
            RpcMsgResp::SeedBalanceResp(balance.to_sat())
        }
        RpcMsgReq::SwapBalance => {
            let balance = maker.get_wallet().read()?.balance_swap_coins(None)?;
            RpcMsgResp::SwapBalanceResp(balance.to_sat())
        }
        RpcMsgReq::NewAddress => {
            let new_address = maker.get_wallet().write()?.get_next_external_address()?;
            RpcMsgResp::NewAddressResp(new_address.to_string())
        }
        RpcMsgReq::SendToAddress {
            address,
            amount,
            fee,
        } => {
            let amount = Amount::from_sat(amount);
            let fee = Amount::from_sat(fee);
            let destination =
                Destination::Address(Address::from_str(&address).unwrap().assume_checked());

            let coins_to_send = maker.get_wallet().read()?.coin_select(amount + fee)?;

            let tx = maker.get_wallet().write()?.spend_from_wallet(
                fee,
                SendAmount::Amount(amount),
                destination,
                &coins_to_send,
            )?;

            let calculated_fee_rate = fee / (tx.weight());
            log::info!("Calculated FeeRate : {:#}", calculated_fee_rate);

            let txid = maker.get_wallet().read()?.send_tx(&tx)?;

            RpcMsgResp::SendToAddressResp(txid.to_string())
        }
        RpcMsgReq::GetDataDir => {
            let path = maker.get_data_dir();
            RpcMsgResp::GetDataDirResp(path.clone())
        }
        RpcMsgReq::GetTorAddress => {
            if maker.config.connection_type == ConnectionType::CLEARNET {
                RpcMsgResp::GetTorAddressResp("Maker is not running on TOR".to_string())
            } else {
                let hostname = get_tor_hostname(&maker.data_dir.join("tor"))?;

                let address = format!("{}:{}", hostname, maker.config.network_port);

                RpcMsgResp::GetTorAddressResp(address)
            }
        }
        RpcMsgReq::Stop => {
            maker.shutdown.store(true, Relaxed);
            RpcMsgResp::Shutdown
        }

        RpcMsgReq::RedeemFidelity(index) => {
            let txid = maker.get_wallet().write()?.redeem_fidelity(index)?;
            RpcMsgResp::FidelitySpend(txid)
        }
        RpcMsgReq::ListFidelity => {
            let list = maker
                .get_wallet()
                .read()?
                .get_fidelity_bonds()
                .iter()
                .map(|(i, (b, _, is_spent))| (*i, (b.clone(), *is_spent)))
                .collect();

            RpcMsgResp::ListBonds(list)
        }
        RpcMsgReq::SyncWallet => {
            log::info!("Initializing wallet sync");
            if let Err(e) = maker.get_wallet().write()?.sync() {
                RpcMsgResp::ServerError(format!("{:?}", e))
            } else {
                log::info!("Completed wallet sync");
                RpcMsgResp::Pong
            }
        }
    };

    if let Err(e) = send_message(socket, &resp) {
        log::error!("Error sending RPC response {:?}", e);
    }

    Ok(())
}

pub(crate) fn start_rpc_server(maker: Arc<Maker>) -> Result<(), MakerError> {
    let rpc_port = maker.config.rpc_port;
    let rpc_socket = format!("127.0.0.1:{}", rpc_port);
    let listener = Arc::new(TcpListener::bind(&rpc_socket)?);
    log::info!(
        "[{}] RPC socket binding successful at {}",
        maker.config.network_port,
        rpc_socket
    );

    listener.set_nonblocking(true)?;

    while !maker.shutdown.load(Relaxed) {
        match listener.accept() {
            Ok((mut stream, addr)) => {
                log::info!("Got RPC request from: {}", addr);
                stream.set_read_timeout(Some(Duration::from_secs(20)))?;
                stream.set_write_timeout(Some(Duration::from_secs(20)))?;
                // Do not cause hard error if a rpc request fails
                if let Err(e) = handle_request(&maker, &mut stream) {
                    log::error!("Error processing RPC Request: {:?}", e);
                    // Send the error back to client.
                    if let Err(e) =
                        send_message(&mut stream, &RpcMsgResp::ServerError(format!("{:?}", e)))
                    {
                        log::error!("Error sending RPC response {:?}", e);
                    };
                }
            }

            Err(e) => {
                if e.kind() == ErrorKind::WouldBlock {
                    // do nothing
                } else {
                    log::error!("Error accepting RPC connection: {:?}", e);
                }
            }
        }

        sleep(HEART_BEAT_INTERVAL);
    }

    Ok(())
}

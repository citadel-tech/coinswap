use std::{
    io::ErrorKind,
    net::{TcpListener, TcpStream},
    sync::Arc,
    thread::sleep,
    time::Duration,
};

use crate::{
    maker::{error::MakerError, rpc::messages::RpcMsgResp, Maker},
    utill::{read_message, send_message},
};

use super::messages::RpcMsgReq;

fn handle_request(maker: &Arc<Maker>, socket: &mut TcpStream) -> Result<(), MakerError> {
    let msg_bytes = read_message(socket)?;
    let rpc_request: RpcMsgReq = serde_cbor::from_slice(&msg_bytes)?;

    match rpc_request {
        RpcMsgReq::Ping => {
            log::info!("RPC request received: {:?}", rpc_request);
            if let Err(e) = send_message(socket, &RpcMsgResp::Pong) {
                log::info!("Error sending RPC response {:?}", e);
            };
        }
        RpcMsgReq::ContractUtxo => {
            let utxos = maker
                .get_wallet()
                .read()?
                .list_live_contract_spend_info(None)?
                .iter()
                .map(|(l, _)| l.clone())
                .collect::<Vec<_>>();
            let resp = RpcMsgResp::ContractUtxoResp { utxos };
            if let Err(e) = send_message(socket, &resp) {
                log::info!("Error sending RPC response {:?}", e);
            };
        }
        RpcMsgReq::FidelityUtxo => {
            let utxos = maker
                .get_wallet()
                .read()?
                .list_fidelity_spend_info(None)?
                .iter()
                .map(|(l, _)| l.clone())
                .collect::<Vec<_>>();
            let resp = RpcMsgResp::FidelityUtxoResp { utxos };
            if let Err(e) = send_message(socket, &resp) {
                log::info!("Error sending RPC response {:?}", e);
            };
        }
        RpcMsgReq::SeedUtxo => {
            let utxos = maker
                .get_wallet()
                .read()?
                .list_descriptor_utxo_spend_info(None)?
                .iter()
                .map(|(l, _)| l.clone())
                .collect::<Vec<_>>();
            let resp = RpcMsgResp::SeedUtxoResp { utxos };
            if let Err(e) = send_message(socket, &resp) {
                log::info!("Error sending RPC response {:?}", e);
            };
        }
        RpcMsgReq::SwapUtxo => {
            let utxos = maker
                .get_wallet()
                .read()?
                .list_swap_coin_utxo_spend_info(None)?
                .iter()
                .map(|(l, _)| l.clone())
                .collect::<Vec<_>>();
            let resp = RpcMsgResp::SwapUtxoResp { utxos };
            if let Err(e) = send_message(socket, &resp) {
                log::info!("Error sending RPC response {:?}", e);
            };
        }
        RpcMsgReq::ContractBalance => {
            let balance = maker.get_wallet().read()?.balance_live_contract(None)?;
            let resp = RpcMsgResp::ContractBalanceResp(balance.to_sat());
            if let Err(e) = send_message(socket, &resp) {
                log::info!("Error sending RPC response {:?}", e);
            };
        }
        RpcMsgReq::FidelityBalance => {
            let balance = maker.get_wallet().read()?.balance_fidelity_bonds(None)?;
            let resp = RpcMsgResp::ContractBalanceResp(balance.to_sat());
            if let Err(e) = send_message(socket, &resp) {
                log::info!("Error sending RPC response {:?}", e);
            };
        }
        RpcMsgReq::SeedBalance => {
            let balance = maker.get_wallet().read()?.balance_descriptor_utxo(None)?;
            let resp = RpcMsgResp::ContractBalanceResp(balance.to_sat());
            if let Err(e) = send_message(socket, &resp) {
                log::info!("Error sending RPC response {:?}", e);
            };
        }
        RpcMsgReq::SwapBalance => {
            let balance = maker.get_wallet().read()?.balance_swap_coins(None)?;
            let resp = RpcMsgResp::ContractBalanceResp(balance.to_sat());
            if let Err(e) = send_message(socket, &resp) {
                log::info!("Error sending RPC response {:?}", e);
            };
        }
        RpcMsgReq::NewAddress => {
            let new_address = maker.get_wallet().write()?.get_next_external_address()?;
            let resp = RpcMsgResp::NewAddressResp(new_address.to_string());
            if let Err(e) = send_message(socket, &resp) {
                log::info!("Error sending RPC response {:?}", e);
            };
        }
    }

    Ok(())
}

pub fn start_rpc_server(maker: Arc<Maker>) -> Result<(), MakerError> {
    let rpc_port = maker.config.rpc_port;
    let rpc_address = format!("127.0.0.1:{}", rpc_port);
    let listener = Arc::new(TcpListener::bind(&rpc_address)?);
    log::info!(
        "[{}] RPC socket binding successful at {}",
        maker.config.port,
        rpc_address
    );

    listener.set_nonblocking(true)?;

    while !*maker.shutdown.read()? {
        match listener.accept() {
            Ok((mut stream, addr)) => {
                log::info!("Got RPC request from: {}", addr);
                stream.set_read_timeout(Some(Duration::from_secs(20)))?;
                stream.set_write_timeout(Some(Duration::from_secs(20)))?;
                handle_request(&maker, &mut stream)?;
            }

            Err(e) => {
                if e.kind() == ErrorKind::WouldBlock {
                    sleep(Duration::from_secs(3));
                    continue;
                } else {
                    log::error!("Error accepting RPC connection: {:?}", e);
                    return Err(e.into());
                }
            }
        }

        sleep(Duration::from_secs(3));
    }

    Ok(())
}

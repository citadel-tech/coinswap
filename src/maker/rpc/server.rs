use std::sync::Arc;

use tokio::{
    io::{AsyncReadExt, BufReader},
    net::{tcp::ReadHalf, TcpListener, TcpStream},
};

use crate::{
    maker::{error::MakerError, rpc::messages::RpcMsgResp, Maker},
    utill::send_message,
};

use super::messages::RpcMsgReq;

/// Reads a RPC Message.
pub async fn read_rpc_message(
    reader: &mut BufReader<ReadHalf<'_>>,
) -> Result<Option<RpcMsgReq>, MakerError> {
    let read_result = reader.read_u32().await;
    // If its EOF, return None
    if read_result
        .as_ref()
        .is_err_and(|e| e.kind() == std::io::ErrorKind::UnexpectedEof)
    {
        return Ok(None);
    }
    let length = read_result?;
    if length == 0 {
        return Ok(None);
    }
    let mut buffer = vec![0; length as usize];
    reader.read_exact(&mut buffer).await?;
    let message: RpcMsgReq = serde_cbor::from_slice(&buffer)?;
    Ok(Some(message))
}

async fn handle_request(maker: &Arc<Maker>, mut socket: TcpStream) -> Result<(), MakerError> {
    let (socket_reader, mut socket_writer) = socket.split();
    let mut reader = BufReader::new(socket_reader);

    if let Some(rpc_request) = read_rpc_message(&mut reader).await? {
        match rpc_request {
            RpcMsgReq::Ping => {
                log::info!("RPC request received: {:?}", rpc_request);
                if let Err(e) = send_message(&mut socket_writer, &RpcMsgResp::Pong).await {
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
                if let Err(e) = send_message(&mut socket_writer, &resp).await {
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
                if let Err(e) = send_message(&mut socket_writer, &resp).await {
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
                if let Err(e) = send_message(&mut socket_writer, &resp).await {
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
                if let Err(e) = send_message(&mut socket_writer, &resp).await {
                    log::info!("Error sending RPC response {:?}", e);
                };
            }
            RpcMsgReq::ContractBalance => {
                let balance = maker.get_wallet().read()?.balance_live_contract(None)?;
                let resp = RpcMsgResp::ContractBalanceResp(balance.to_sat());
                if let Err(e) = send_message(&mut socket_writer, &resp).await {
                    log::info!("Error sending RPC response {:?}", e);
                };
            }
            RpcMsgReq::FidelityBalance => {
                let balance = maker.get_wallet().read()?.balance_fidelity_bonds(None)?;
                let resp = RpcMsgResp::ContractBalanceResp(balance.to_sat());
                if let Err(e) = send_message(&mut socket_writer, &resp).await {
                    log::info!("Error sending RPC response {:?}", e);
                };
            }
            RpcMsgReq::SeedBalance => {
                let balance = maker.get_wallet().read()?.balance_descriptor_utxo(None)?;
                let resp = RpcMsgResp::ContractBalanceResp(balance.to_sat());
                if let Err(e) = send_message(&mut socket_writer, &resp).await {
                    log::info!("Error sending RPC response {:?}", e);
                };
            }
            RpcMsgReq::SwapBalance => {
                let balance = maker.get_wallet().read()?.balance_swap_coins(None)?;
                let resp = RpcMsgResp::ContractBalanceResp(balance.to_sat());
                if let Err(e) = send_message(&mut socket_writer, &resp).await {
                    log::info!("Error sending RPC response {:?}", e);
                };
            }
        }
    }

    Ok(())
}

pub async fn start_rpc_server_thread(maker: Arc<Maker>) {
    let rpc_port = maker.config.rpc_port;
    let rpc_socket = format!("127.0.0.1:{}", rpc_port);
    let listener = TcpListener::bind(&rpc_socket).await.unwrap();
    log::info!(
        "[{}] RPC socket binding successful at {}",
        maker.config.port,
        rpc_socket
    );
    tokio::spawn(async move {
        loop {
            let (socket, addrs) = listener.accept().await.unwrap();
            log::info!("Got RPC request from: {}", addrs);
            handle_request(&maker, socket).await.unwrap();
        }
    });
}

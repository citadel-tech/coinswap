use crate::taker::{
    error::TakerError,
    rpc::messages::{RpcMsgReq, RpcMsgResp},
    Taker,
};
use bitcoin::Network;
use serde::{de::DeserializeOwned, Serialize};
use std::{
    io,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, RwLock},
};
// use tokio::{
//     io::{AsyncReadExt, BufReader},
//     net::{tcp::ReadHalf, TcpListener, TcpStream},
// };
use crate::taker::SwapParams;

pub fn start_taker_rpc_server(
    taker: Arc<RwLock<Taker>>,
    network: Network,
    swap_params: SwapParams,
) {
    let rpc_port = taker.read().unwrap().config.rpc_port;
    let rpc_socket = format!("127.0.0.1:{}", rpc_port);
    let listener = TcpListener::bind(&rpc_socket).unwrap();
    log::info!(
        "[{}] RPC socket binding successfull at {}",
        taker.read().unwrap().config.rpc_port,
        rpc_socket
    );

    loop {
        let (socket, addrs) = listener.accept().unwrap();
        log::info!("Got RPC request from: {}", addrs);
        handle_request(&taker, &network, &swap_params, socket).unwrap();
    }
}

fn handle_request(
    taker: &Arc<RwLock<Taker>>,
    network: &Network,
    swap_params: &SwapParams,
    mut stream: TcpStream,
) -> Result<(), TakerError> {
    if let Some(rpc_request) = read_rpc_message(&mut stream)? {
        match rpc_request {
            RpcMsgReq::Ping => {
                log::info!("RPC request received :{:?}", rpc_request);
                if let Err(e) = send_message(&mut stream, &RpcMsgResp::Pong) {
                    log::error!("Error sending RPC response {:?}", e);
                }
            }
            RpcMsgReq::ContractUtxo => {
                let utxos = taker
                    .read()
                    .unwrap()
                    .get_wallet()
                    .list_live_contract_spend_info(None)?
                    .iter()
                    .map(|(l, _)| l.clone())
                    .collect();
                let resp = RpcMsgResp::ContractUtxoResp { utxos };
                if let Err(e) = send_message(&mut stream, &resp) {
                    log::error!("Error sending RPC response {:?}", e);
                }
            }
            RpcMsgReq::FidelityUtxo => {
                let utxos = taker
                    .read()
                    .unwrap()
                    .get_wallet()
                    .list_fidelity_spend_info(None)?
                    .iter()
                    .map(|(l, _)| l.clone())
                    .collect();
                let resp = RpcMsgResp::FidelityResp { utxos };
                if let Err(e) = send_message(&mut stream, &resp) {
                    log::error!("Error sending RPC response {:?}", e);
                }
            }
            RpcMsgReq::SeedUtxo => {
                let utxos = taker
                    .read()
                    .unwrap()
                    .get_wallet()
                    .list_descriptor_utxo_spend_info(None)?
                    .iter()
                    .map(|(l, _)| l.clone())
                    .collect();
                let resp = RpcMsgResp::SeedUtxoResp { utxos };
                if let Err(e) = send_message(&mut stream, &resp) {
                    log::error!("Error sending RPC response {:?}", e);
                }
            }
            RpcMsgReq::SwapUtxo => {
                let utxos = taker
                    .read()
                    .unwrap()
                    .get_wallet()
                    .list_swap_coin_utxo_spend_info(None)?
                    .iter()
                    .map(|(l, _)| l.clone())
                    .collect();
                let resp = RpcMsgResp::SwapUtxoResp { utxos };
                if let Err(e) = send_message(&mut stream, &resp) {
                    log::error!("Error sending RPC response {:?}", e);
                }
            }
            RpcMsgReq::ContractBalance => {
                let balance = taker
                    .read()
                    .unwrap()
                    .get_wallet()
                    .balance_live_contract(None)?;
                let resp = RpcMsgResp::ContractBalanceResp(balance.to_sat());
                if let Err(e) = send_message(&mut stream, &resp) {
                    log::error!("Error sending RPC response {:?}", e);
                }
            }
            RpcMsgReq::FidelityBalance => {
                let balance = taker
                    .read()
                    .unwrap()
                    .get_wallet()
                    .balance_fidelity_bonds(None)?;
                let resp = RpcMsgResp::FidelityBalanceResp(balance.to_sat());
                if let Err(e) = send_message(&mut stream, &resp) {
                    log::error!("Error sending RPC response {:?}", e);
                }
            }
            RpcMsgReq::SeedBalance => {
                let balance = taker
                    .read()
                    .unwrap()
                    .get_wallet()
                    .balance_descriptor_utxo(None)?;
                let resp = RpcMsgResp::SeedBalanceResp(balance.to_sat());
                if let Err(e) = send_message(&mut stream, &resp) {
                    log::error!("Error sending RPC response {:?}", e);
                }
            }
            RpcMsgReq::SwapBalance => {
                let balance = taker
                    .read()
                    .unwrap()
                    .get_wallet()
                    .balance_swap_coins(None)?;
                let resp = RpcMsgResp::SeedBalanceResp(balance.to_sat());
                if let Err(e) = send_message(&mut stream, &resp) {
                    log::error!("Error sending RPC response {:?}", e);
                }
            }
            RpcMsgReq::TotalBalance => {
                let balance = taker.read().unwrap().get_wallet().balance()?;
                let resp = RpcMsgResp::TotalBalanceResp(balance.to_sat());
                if let Err(e) = send_message(&mut stream, &resp) {
                    log::error!("Error sending RPC response {:?}", e);
                }
            }
            RpcMsgReq::SyncOfferBook => {
                let config = taker.write().unwrap().config.clone();
                let _ = futures::executor::block_on(taker.write().unwrap().sync_offerbook(
                    *network,
                    &config,
                    swap_params.maker_count,
                ));
                let resp = RpcMsgResp::SyncOfferBook;
                if let Err(e) = send_message(&mut stream, &resp) {
                    log::error!("Error sending RPC response {:?}", e);
                }
            }
            RpcMsgReq::GetNewAddress => {
                let address = taker
                    .write()
                    .unwrap()
                    .get_wallet_mut()
                    .get_next_external_address()?;
                let resp = RpcMsgResp::GetNewAddressResp(address.to_string());
                if let Err(e) = send_message(&mut stream, &resp) {
                    log::error!("Error sending RPC response {:?}", e);
                }
            }
            RpcMsgReq::DoCoinswap => {
                let _ = taker.write().unwrap().do_coinswap(*swap_params);
                let resp = RpcMsgResp::DoCoinswap;
                if let Err(e) = send_message(&mut stream, &resp) {
                    log::error!("Error sending RPC response {:?}", e);
                }
            }
        }
    }
    Ok(())
}

fn read_rpc_message<T: DeserializeOwned>(stream: &mut TcpStream) -> Result<Option<T>, TakerError> {
    let mut length_buf = [0u8; 4];
    if let Err(e) = stream.read_exact(&mut length_buf) {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            return Ok(None);
        } else {
            return Err(e.into());
        }
    }

    let length = u32::from_be_bytes(length_buf) as usize;
    if length == 0 {
        return Ok(None);
    }

    let mut buffer = vec![0u8; length];
    stream.read_exact(&mut buffer)?;
    let message = serde_cbor::from_slice(&buffer)?;
    Ok(Some(message))
}

fn send_message<T: Serialize>(stream: &mut TcpStream, message: &T) -> Result<(), TakerError> {
    let message_cbor = serde_cbor::to_vec(message)?;
    let length = (message_cbor.len() as u32).to_be_bytes();
    stream.write_all(&length)?;
    stream.write_all(&message_cbor)?;
    stream.flush()?;
    Ok(())
}

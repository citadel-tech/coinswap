use std::{
    fs::File,
    io::{ErrorKind, Read},
    net::{TcpListener, TcpStream},
    sync::{atomic::Ordering::Relaxed, Arc},
    thread::sleep,
    time::Duration,
};

use bitcoin::{Address, Amount};

use super::messages::RpcMsgReq;
use crate::{
    maker::{error::MakerError, rpc::messages::RpcMsgResp, Maker},
    utill::{read_message, send_message, ConnectionType, HEART_BEAT_INTERVAL},
    wallet::{Destination, SendAmount},
};
use std::str::FromStr;

fn handle_request(maker: &Arc<Maker>, socket: &mut TcpStream) -> Result<(), MakerError> {
    let msg_bytes = read_message(socket)?;
    let rpc_request: RpcMsgReq = serde_cbor::from_slice(&msg_bytes)?;
    log::info!("RPC request received: {:?}", rpc_request);

    match rpc_request {
        RpcMsgReq::Ping => {
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
            let resp = RpcMsgResp::FidelityBalanceResp(balance.to_sat());
            if let Err(e) = send_message(socket, &resp) {
                log::info!("Error sending RPC response {:?}", e);
            };
        }
        RpcMsgReq::SeedBalance => {
            let balance = maker.get_wallet().read()?.balance_descriptor_utxo(None)?;
            let resp = RpcMsgResp::SeedBalanceResp(balance.to_sat());
            if let Err(e) = send_message(socket, &resp) {
                log::info!("Error sending RPC response {:?}", e);
            };
        }
        RpcMsgReq::SwapBalance => {
            let balance = maker.get_wallet().read()?.balance_swap_coins(None)?;
            let resp = RpcMsgResp::SwapBalanceResp(balance.to_sat());
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

            let resp =
                RpcMsgResp::SendToAddressResp(bitcoin::consensus::encode::serialize_hex(&tx));
            if let Err(e) = send_message(socket, &resp) {
                log::info!("Error sending RPC response {:?}", e);
            };
        }
        RpcMsgReq::GetDataDir => {
            let path = maker.get_data_dir();
            let resp = RpcMsgResp::GetDataDirResp(path.clone());
            if let Err(e) = send_message(socket, &resp) {
                log::info!("Error sending RPC response {:?}", e);
            };
        }
        RpcMsgReq::GetTorAddress => {
            if maker.config.connection_type == ConnectionType::CLEARNET {
                let resp = RpcMsgResp::GetTorAddressResp("Maker is not running on TOR".to_string());
                if let Err(e) = send_message(socket, &resp) {
                    log::info!("Error sending RPC response {:?}", e);
                };
            } else {
                let maker_hs_path_str =
                    format!("/tmp/tor-rust-maker{}/hs-dir/hostname", maker.config.port);
                let mut maker_file = File::open(maker_hs_path_str)?;
                let mut maker_onion_addr: String = String::new();
                maker_file.read_to_string(&mut maker_onion_addr)?;
                maker_onion_addr.pop(); // Remove `\n` at the end.
                let maker_address = format!("{}:{}", maker_onion_addr, maker.config.port);

                let resp = RpcMsgResp::GetTorAddressResp(maker_address);
                if let Err(e) = send_message(socket, &resp) {
                    log::info!("Error sending RPC response {:?}", e);
                };
            }
        }
        RpcMsgReq::Stop => {
            maker.shutdown.store(true, Relaxed);
            if let Err(e) = send_message(socket, &RpcMsgResp::Shutdown) {
                log::info!("Error sending RPC response {:?}", e);
            };
        }
    }

    Ok(())
}

pub fn start_rpc_server(maker: Arc<Maker>) -> Result<(), MakerError> {
    let rpc_port = maker.config.rpc_port;
    let rpc_socket = format!("127.0.0.1:{}", rpc_port);
    let listener = Arc::new(TcpListener::bind(&rpc_socket)?);
    log::info!(
        "[{}] RPC socket binding successful at {}",
        maker.config.port,
        rpc_socket
    );

    listener.set_nonblocking(true)?;

    while !maker.shutdown.load(Relaxed) {
        match listener.accept() {
            Ok((mut stream, addr)) => {
                log::info!("Got RPC request from: {}", addr);
                stream.set_read_timeout(Some(Duration::from_secs(20)))?;
                stream.set_write_timeout(Some(Duration::from_secs(20)))?;
                handle_request(&maker, &mut stream)?;
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

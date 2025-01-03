use std::{
    fs::File,
    io::{ErrorKind, Read},
    net::{TcpListener, TcpStream},
    sync::{atomic::Ordering::Relaxed, Arc},
    thread::sleep,
    time::Duration,
};

use bitcoin::{Address, Amount};
use bitcoind::bitcoincore_rpc::RpcApi;

use super::messages::RpcMsgReq;
use crate::{
    maker::{error::MakerError, rpc::messages::RpcMsgResp, Maker},
    utill::{read_message, send_message, ConnectionType, HEART_BEAT_INTERVAL},
    wallet::{Destination, SendAmount, WalletError},
};
use std::str::FromStr;

fn handle_request(maker: &Arc<Maker>, socket: &mut TcpStream) -> Result<(), MakerError> {
    let msg_bytes = read_message(socket)?;
    let rpc_request: RpcMsgReq = serde_cbor::from_slice(&msg_bytes)?;
    log::info!("RPC request received: {:?}", rpc_request);

    match rpc_request {
        RpcMsgReq::Ping => {
            if let Err(e) = send_message(socket, &RpcMsgResp::Pong) {
                log::error!("Error sending RPC response {:?}", e);
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
                log::error!("Error sending RPC response {:?}", e);
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
                log::error!("Error sending RPC response {:?}", e);
            };
        }
        RpcMsgReq::Utxo => {
            let utxos = maker
                .get_wallet()
                .read()?
                .list_all_utxo_spend_info(None)?
                .iter()
                .map(|(l, _)| l.clone())
                .collect::<Vec<_>>();
            let resp = RpcMsgResp::UtxoResp { utxos };
            if let Err(e) = send_message(socket, &resp) {
                log::error!("Error sending RPC response {:?}", e);
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
                log::error!("Error sending RPC response {:?}", e);
            };
        }
        RpcMsgReq::ContractBalance => {
            let balance = maker.get_wallet().read()?.balance_live_contract(None)?;
            let resp = RpcMsgResp::ContractBalanceResp(balance.to_sat());
            if let Err(e) = send_message(socket, &resp) {
                log::error!("Error sending RPC response {:?}", e);
            };
        }
        RpcMsgReq::FidelityBalance => {
            let balance = maker.get_wallet().read()?.balance_fidelity_bonds(None)?;
            let resp = RpcMsgResp::FidelityBalanceResp(balance.to_sat());
            if let Err(e) = send_message(socket, &resp) {
                log::error!("Error sending RPC response {:?}", e);
            };
        }
        RpcMsgReq::Balance => {
            let balance = maker.get_wallet().read()?.spendable_balance()?;
            let resp = RpcMsgResp::SeedBalanceResp(balance.to_sat());
            if let Err(e) = send_message(socket, &resp) {
                log::error!("Error sending RPC response {:?}", e);
            };
        }
        RpcMsgReq::SwapBalance => {
            let balance = maker.get_wallet().read()?.balance_swap_coins(None)?;
            let resp = RpcMsgResp::SwapBalanceResp(balance.to_sat());
            if let Err(e) = send_message(socket, &resp) {
                log::error!("Error sending RPC response {:?}", e);
            };
        }
        RpcMsgReq::NewAddress => {
            let new_address = maker.get_wallet().write()?.get_next_external_address()?;
            let resp = RpcMsgResp::NewAddressResp(new_address.to_string());
            if let Err(e) = send_message(socket, &resp) {
                log::error!("Error sending RPC response {:?}", e);
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

            let txid = maker
                .get_wallet()
                .read()?
                .rpc
                .send_raw_transaction(&tx)
                .map_err(WalletError::Rpc)?;

            let resp = RpcMsgResp::SendToAddressResp(txid.to_string());
            if let Err(e) = send_message(socket, &resp) {
                log::error!("Error sending RPC response {:?}", e);
            };
        }
        RpcMsgReq::GetDataDir => {
            let path = maker.get_data_dir();
            let resp = RpcMsgResp::GetDataDirResp(path.clone());
            if let Err(e) = send_message(socket, &resp) {
                log::error!("Error sending RPC response {:?}", e);
            };
        }
        RpcMsgReq::GetTorAddress => {
            if maker.config.connection_type == ConnectionType::CLEARNET {
                let resp = RpcMsgResp::GetTorAddressResp("Maker is not running on TOR".to_string());
                if let Err(e) = send_message(socket, &resp) {
                    log::error!("Error sending RPC response {:?}", e);
                };
            } else {
                let maker_hs_path_str = format!(
                    "/tmp/tor-rust-maker{}/hs-dir/hostname",
                    maker.config.network_port
                );
                let mut maker_file = File::open(maker_hs_path_str)?;
                let mut maker_onion_addr: String = String::new();
                maker_file.read_to_string(&mut maker_onion_addr)?;
                maker_onion_addr.pop(); // Remove `\n` at the end.
                let maker_address = format!("{}:{}", maker_onion_addr, maker.config.network_port);

                let resp = RpcMsgResp::GetTorAddressResp(maker_address);
                if let Err(e) = send_message(socket, &resp) {
                    log::error!("Error sending RPC response {:?}", e);
                };
            }
        }
        RpcMsgReq::Stop => {
            maker.shutdown.store(true, Relaxed);
            if let Err(e) = send_message(socket, &RpcMsgResp::Shutdown) {
                log::error!("Error sending RPC response {:?}", e);
            };
        }

        RpcMsgReq::RedeemFidelity(index) => {
            let txid = maker.get_wallet().write()?.redeem_fidelity(index)?;
            if let Err(e) = send_message(socket, &RpcMsgResp::FidelitySpend(txid)) {
                log::error!("Error sending RPC response {:?}", e);
            };
        }
        RpcMsgReq::ListFidelity => {
            let list = maker
                .get_wallet()
                .read()?
                .get_fidelity_bonds()
                .iter()
                .map(|(i, (b, _, is_spent))| (*i, (b.clone(), *is_spent)))
                .collect();
            if let Err(e) = send_message(socket, &RpcMsgResp::ListBonds(list)) {
                log::error!("Error sending RPC response {:?}", e);
            };
        }
        RpcMsgReq::SyncWallet => {
            log::info!("Starting wallet sync.");
            let resp = if let Err(e) = maker.get_wallet().write()?.sync() {
                RpcMsgResp::ServerError(format!("{:?}", e))
            } else {
                log::info!("Wallet sync success.");
                RpcMsgResp::Pong
            };

            if let Err(e) = send_message(socket, &resp) {
                log::error!("Error sending RPC response {:?}", e);
            };
        }
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

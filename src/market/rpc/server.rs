use bitcoin::OutPoint;

use super::{RpcMsgReq, RpcMsgResp};
use crate::{
    error::NetError,
    market::directory::{DirectoryServer, DirectoryServerError},
    utill::{read_message, send_message, HEART_BEAT_INTERVAL},
};
use std::{
    collections::{BTreeSet, HashMap},
    io::ErrorKind,
    net::{TcpListener, TcpStream},
    sync::{atomic::Ordering::Relaxed, Arc, RwLock},
    thread::sleep,
    time::{Duration, Instant},
};
fn handle_request(
    socket: &mut TcpStream,
    address: Arc<RwLock<HashMap<OutPoint, (String, Instant)>>>,
) -> Result<(), DirectoryServerError> {
    let req_bytes = read_message(socket)?;
    let rpc_request: RpcMsgReq = serde_cbor::from_slice(&req_bytes).map_err(NetError::Cbor)?;

    match rpc_request {
        RpcMsgReq::ListAddresses => {
            log::info!("RPC request received: {:?}", rpc_request);
            let resp = RpcMsgResp::ListAddressesResp(
                address
                    .read()?
                    .iter()
                    .map(|(op, address)| (*op, address.0.clone()))
                    .collect::<BTreeSet<_>>(),
            );
            send_message(socket, &resp)?;
        }
    }

    Ok(())
}

pub fn start_rpc_server_thread(
    directory: Arc<DirectoryServer>,
) -> Result<(), DirectoryServerError> {
    let rpc_port = directory.rpc_port;
    let rpc_socket = format!("127.0.0.1:{}", rpc_port);
    let listener = Arc::new(TcpListener::bind(&rpc_socket)?);
    log::info!("RPC socket binding successful at {}", rpc_socket);

    listener.set_nonblocking(true)?;

    while !directory.shutdown.load(Relaxed) {
        match listener.accept() {
            Ok((mut stream, addr)) => {
                log::info!("Got RPC request from: {}", addr);
                stream.set_read_timeout(Some(Duration::from_secs(20)))?;
                stream.set_write_timeout(Some(Duration::from_secs(20)))?;
                if let Err(e) = handle_request(&mut stream, directory.addresses.clone()) {
                    log::error!("Error handling RPC request: {:?}", e);
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
        // use heart_beat
        sleep(HEART_BEAT_INTERVAL);
    }

    Ok(())
}

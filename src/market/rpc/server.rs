use super::{RpcMsgReq, RpcMsgResp};
use crate::{
    error::NetError,
    market::directory::{AddressEntry, DirectoryServer, DirectoryServerError},
    utill::{read_message, send_message},
};
use std::{
    collections::BTreeSet,
    io::ErrorKind,
    net::{TcpListener, TcpStream},
    sync::{atomic::Ordering::Relaxed, Arc, RwLock},
    thread::sleep,
    time::Duration,
};

fn handle_request(
    socket: &mut TcpStream,
    address: Arc<RwLock<BTreeSet<AddressEntry>>>,
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
                    .map(|AddressEntry(_, address)| address.clone())
                    .collect::<BTreeSet<_>>(),
            );
            if let Err(e) = send_message(socket, &resp) {
                log::info!("Error sending RPC response {:?}", e);
            };
        }
    }

    Ok(())
}

pub fn start_rpc_server(
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
                handle_request(&mut stream, directory.addresses.clone())?;
            }
            Err(e) => {
                if e.kind() == ErrorKind::WouldBlock {
                    sleep(Duration::from_secs(3));
                    continue;
                } else {
                    log::error!("Error accepting RPC connection: {:?}", e);
                    break;
                }
            }
        }
        sleep(Duration::from_secs(3));
    }

    Ok(())
}

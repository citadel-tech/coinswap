use std::{net::TcpStream, time::Duration};

use clap::Parser;

use coinswap::{
    error::NetError,
    market::{
        directory::DirectoryServerError,
        rpc::{RpcMsgReq, RpcMsgResp},
    },
    utill::{read_message, send_message, setup_directory_logger},
};

/// directory-cli is a command line app to send RPC messages to directory server.
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct App {
    /// The command to execute
    #[clap(subcommand)]
    command: Commands,
}

#[derive(Parser, Debug)]
enum Commands {
    /// Lists all the addresses from the directory server
    ListAddresses,
}

fn send_rpc_req(req: &RpcMsgReq) -> Result<(), DirectoryServerError> {
    let mut stream = TcpStream::connect("127.0.0.1:4321")?;
    stream.set_read_timeout(Some(Duration::from_secs(20)))?;
    stream.set_write_timeout(Some(Duration::from_secs(20)))?;

    send_message(&mut stream, &req)?;

    let resp_bytes = read_message(&mut stream)?;
    let resp: RpcMsgResp = serde_cbor::from_slice(&resp_bytes).map_err(NetError::Cbor)?;

    println!("{:?}", resp);
    Ok(())
}

fn main() -> Result<(), DirectoryServerError> {
    setup_directory_logger(log::LevelFilter::Info);
    let cli = App::parse();

    match cli.command {
        Commands::ListAddresses => {
            send_rpc_req(&RpcMsgReq::ListAddresses)?;
        }
    }
    Ok(())
}

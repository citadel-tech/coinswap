use std::{net::TcpStream, time::Duration};

use clap::Parser;

use coinswap::{
    error::AppError,
    maker::MakerError,
    market::rpc::{RpcMsgReq, RpcMsgResp},
    utill::{read_message, send_message, setup_logger},
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

fn send_rpc_req(req: &RpcMsgReq) -> Result<(), AppError> {
    let mut stream = TcpStream::connect("127.0.0.1:4321")
        .map_err(MakerError::IO)?;
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .map_err(MakerError::IO)?;
    stream
        .set_write_timeout(Some(Duration::from_secs(20)))
        .map_err(MakerError::IO)?;

    send_message(&mut stream, &req)
        .map_err(MakerError::Net)?;

    let resp_bytes = read_message(&mut stream)
        .map_err(MakerError::Net)?;
    let resp: RpcMsgResp = serde_cbor::from_slice(&resp_bytes)
        .map_err(MakerError::Deserialize)?;

    println!("{:?}", resp);
    Ok(())
}

fn main() -> Result<(), AppError>{
    setup_logger();
    let cli = App::parse();

    match cli.command {
        Commands::ListAddresses => {
            send_rpc_req(&RpcMsgReq::ListAddresses)?;
        }
    }
    Ok(())
}
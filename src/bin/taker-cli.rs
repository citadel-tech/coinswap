use clap::Parser;
use coinswap::{
    taker::{error::TakerError, rpc::RpcMsgReq},
    utill::setup_logger,
};
use serde::{Deserialize, Serialize};
// use std::{
//     error::Error,
//     fmt, io,
//     io::{Read, Write},
//     net::TcpStream,
// };
use coinswap::utill::send_message;
use tokio::{
    io::{AsyncReadExt, BufReader},
    net::tcp::ReadHalf,
};

#[derive(Serialize, Deserialize, Debug)]
enum Message {
    Hello,
}

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct App {
    /// The command to execute
    #[clap(subcommand)]
    command: Commands,
}

#[derive(Parser, Debug)]
enum Commands {
    Ping,
    SeedUtxo,
    SwapUtxo,
    ContractUtxo,
    FidelityUtxo,
    SeedBalance,
    SwapBalance,
    ContractBalance,
    FidelityBalance,
    TotalBalance,
    GetNewAddress,
    SyncOfferBook,
    DoCoinswap,
}

#[tokio::main]
async fn main() -> Result<(), TakerError> {
    setup_logger();
    let cli = App::parse();
    match cli.command {
        Commands::Ping => {
            send_rpc_req(&RpcMsgReq::Ping).await?;
        }
        Commands::ContractUtxo => {
            send_rpc_req(&RpcMsgReq::ContractUtxo).await?;
        }
        Commands::SwapUtxo => send_rpc_req(&RpcMsgReq::SwapUtxo).await?,
        Commands::SeedUtxo => send_rpc_req(&RpcMsgReq::SeedUtxo).await?,
        Commands::FidelityUtxo => send_rpc_req(&RpcMsgReq::FidelityUtxo).await?,
        Commands::ContractBalance => send_rpc_req(&RpcMsgReq::ContractBalance).await?,
        Commands::SwapBalance => send_rpc_req(&RpcMsgReq::SwapBalance).await?,
        Commands::SeedBalance => send_rpc_req(&RpcMsgReq::SeedUtxo).await?,
        Commands::FidelityBalance => send_rpc_req(&RpcMsgReq::FidelityBalance).await?,
        Commands::TotalBalance => send_rpc_req(&RpcMsgReq::TotalBalance).await?,
        Commands::GetNewAddress => send_rpc_req(&RpcMsgReq::GetNewAddress).await?,
        Commands::SyncOfferBook => send_rpc_req(&RpcMsgReq::SyncOfferBook).await?,
        Commands::DoCoinswap => send_rpc_req(&RpcMsgReq::DoCoinswap).await?,
    }
    Ok(())
}

async fn send_rpc_req(req: &RpcMsgReq) -> Result<(), TakerError> {
    let mut stream = tokio::net::TcpStream::connect("127.0.0.1:8081").await?;

    let (read_half, mut write_half) = stream.split();

    if let Err(e) = send_message(&mut write_half, &req).await {
        log::error!("Error Sending RPC message : {:?}", e);
    };

    let mut read_half = BufReader::new(read_half);

    if let Some(rpc_resp) = read_rpc_message(&mut read_half).await? {
        println!("{:?}", rpc_resp);
    } else {
        log::error!("No RPC response received");
    }

    Ok(())
}

pub async fn read_rpc_message(
    reader: &mut BufReader<ReadHalf<'_>>,
) -> Result<Option<RpcMsgReq>, TakerError> {
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

// #[derive(Debug)]
// enum TakerRpcError {
//     IoError(io::Error),
//     CborError(serde_cbor::Error),
// }
//
// impl Error for TakerRpcError {
//     fn source(&self) -> Option<&(dyn Error + 'static)> {
//         match *self {
//             TakerRpcError::IoError(ref e) => Some(e),
//             TakerRpcError::CborError(ref e) => Some(e),
//         }
//     }
// }
//
// impl fmt::Display for TakerRpcError {
//     fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
//         match *self {
//             TakerRpcError::IoError(ref err) => write!(f, "IO Error: {}", err),
//             TakerRpcError::CborError(ref err) => write!(f, "CBOR Serialization Error: {}", err),
//         }
//     }
// }
// impl From<io::Error> for TakerRpcError {
//     fn from(err: io::Error) -> Self {
//         TakerRpcError::IoError(err)
//     }
// }
//
// impl From<serde_cbor::Error> for TakerRpcError {
//     fn from(err: serde_cbor::Error) -> Self {
//         TakerRpcError::CborError(err)
//     }
// }
//
// fn send_message<T: Serialize>(stream: &mut TcpStream, message: &T) -> Result<(), TakerRpcError> {
//     let message_cbor = serde_cbor::to_vec(message)?;
//     let length = (message_cbor.len() as u32).to_be_bytes();
//     stream.write_all(&length)?;
//     stream.write_all(&message_cbor)?;
//     stream.flush()?;
//     Ok(())
// }
//
// fn read_rpc_message<T: DeserializeOwned>(
//     stream: &mut TcpStream,
// ) -> Result<Option<T>, TakerRpcError> {
//     let mut length_buf = [0u8; 4];
//     if let Err(e) = stream.read_exact(&mut length_buf) {
//         if e.kind() == io::ErrorKind::UnexpectedEof {
//             return Ok(None);
//         } else {
//             return Err(e.into());
//         }
//     }
//
//     let length = u32::from_be_bytes(length_buf) as usize;
//     if length == 0 {
//         return Ok(None);
//     }
//
//     let mut buffer = vec![0u8; length];
//     stream.read_exact(&mut buffer)?;
//     let message = serde_cbor::from_slice(&buffer)?;
//     Ok(Some(message))
// }
//
// fn send_rpc_req(req: &RpcMsgReq) {
//     let mut stream = match TcpStream::connect("127.0.0.1:8081") {
//         Ok(stream) => stream,
//         Err(e) => {
//             log::error!("Failed to connect: {:?}", e);
//             return;
//         }
//     };
//
//     if let Err(e) = send_message(&mut stream, req) {
//         log::error!("Failed to send message: {:?}", e);
//         return;
//     }
//
//     match read_rpc_message::<RpcMsgResp>(&mut stream) {
//         Ok(Some(response)) => log::info!("Received response: {:?}", response),
//         Ok(None) => log::info!("No response received"),
//         Err(e) => log::error!("Failed to read response: {:?}", e),
//     }
// }

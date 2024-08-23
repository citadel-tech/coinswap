use std::{net::TcpStream, time::Duration};

use clap::Parser;
use coinswap::{
    error::AppError,
    maker::{MakerError, RpcMsgReq, RpcMsgResp},
    utill::{read_message, send_message, setup_logger},
};

/// maker-cli is a command line app to send RPC messages to maker server.
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct App {
    /// The command to execute
    #[clap(subcommand)]
    command: Commands,
}

#[derive(Parser, Debug)]
enum Commands {
    /// Sends a Ping
    Ping,
    /// Returns a list of seed utxos
    SeedUtxo,
    /// Returns a list of swap coin utxos
    SwapUtxo,
    /// Returns a list of live contract utxos
    ContractUtxo,
    /// Returns a list of fidelity utxos
    FidelityUtxo,
    /// Returns the total seed balance
    SeedBalance,
    /// Returns the total swap coin balance
    SwapBalance,
    /// Returns the total live contract balance
    ContractBalance,
    /// Returns the total fidelity balance
    FidelityBalance,
    /// Gets a new address
    NewAddress,
}

fn main() -> Result<(), AppError> {
    setup_logger();
    let cli = App::parse();

    match cli.command {
        Commands::Ping => {
            send_rpc_req(&RpcMsgReq::Ping)?;
        }
        Commands::ContractUtxo => {
            send_rpc_req(&RpcMsgReq::ContractUtxo)?;
        }
        Commands::ContractBalance => {
            send_rpc_req(&RpcMsgReq::ContractBalance)?;
        }
        Commands::FidelityBalance => {
            send_rpc_req(&RpcMsgReq::FidelityBalance)?;
        }
        Commands::FidelityUtxo => {
            send_rpc_req(&RpcMsgReq::FidelityUtxo)?;
        }
        Commands::SeedBalance => {
            send_rpc_req(&RpcMsgReq::SeedBalance)?;
        }
        Commands::SeedUtxo => {
            send_rpc_req(&RpcMsgReq::SeedUtxo)?;
        }
        Commands::SwapBalance => {
            send_rpc_req(&RpcMsgReq::SwapBalance)?;
        }
        Commands::SwapUtxo => {
            send_rpc_req(&RpcMsgReq::SwapUtxo)?;
        }
        Commands::NewAddress => {
            send_rpc_req(&RpcMsgReq::NewAddress)?;
        }
    }

    Ok(())
}

fn send_rpc_req(req: &RpcMsgReq) -> Result<(), AppError> {
    let mut stream = TcpStream::connect("127.0.0.1:8080")
        .map_err(MakerError::IO)?;
    stream.set_read_timeout(Some(Duration::from_secs(20)))
        .map_err(MakerError::IO)?;
    stream.set_write_timeout(Some(Duration::from_secs(20)))
        .map_err(MakerError::IO)?;

    send_message(&mut stream, &req)
        .map_err(MakerError::Net)?;

    let response_bytes = read_message(&mut stream)
        .map_err(MakerError::Net)?;
    let response: RpcMsgResp = serde_cbor::from_slice(&response_bytes)
        .map_err(MakerError::Deserialize)?;

    println!("{:?}", response);

    Ok(())
}

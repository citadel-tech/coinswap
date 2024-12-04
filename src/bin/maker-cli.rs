use std::{net::TcpStream, time::Duration};

use clap::Parser;
use coinswap::{
    maker::{MakerError, RpcMsgReq, RpcMsgResp},
    utill::{read_message, send_message, setup_maker_logger},
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
    // Send to an external wallet address.
    SendToAddress {
        address: String,
        amount: u64,
        fee: u64,
    },
    /// Returns the tor address
    GetTorAddress,
    /// Returns the data dir
    GetDataDir,
}

fn main() -> Result<(), MakerError> {
    setup_maker_logger(log::LevelFilter::Info);
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
        Commands::SendToAddress {
            address,
            amount,
            fee,
        } => {
            send_rpc_req(&RpcMsgReq::SendToAddress {
                address,
                amount,
                fee,
            })?;
        }
        Commands::GetTorAddress => {
            send_rpc_req(&RpcMsgReq::GetTorAddress)?;
        }
        Commands::GetDataDir => {
            send_rpc_req(&RpcMsgReq::GetDataDir)?;
        }
    }

    Ok(())
}

fn send_rpc_req(req: &RpcMsgReq) -> Result<(), MakerError> {
    let mut stream = TcpStream::connect("127.0.0.1:6103")?;
    stream.set_read_timeout(Some(Duration::from_secs(20)))?;
    stream.set_write_timeout(Some(Duration::from_secs(20)))?;

    send_message(&mut stream, &req)?;

    let response_bytes = read_message(&mut stream)?;
    let response: RpcMsgResp = serde_cbor::from_slice(&response_bytes)?;

    println!("{:?}", response);

    Ok(())
}

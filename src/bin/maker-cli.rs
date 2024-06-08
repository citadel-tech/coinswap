use clap::Parser;
use coinswap::{
    maker::{
        error::MakerError,
        rpc::{read_rpc_message, RpcMsgReq},
    },
    utill::{send_message, setup_logger},
};
use serde::{Deserialize, Serialize};
use tokio::{io::BufReader, net::TcpStream};

#[derive(Serialize, Deserialize, Debug)]
enum Message {
    Hello,
}

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
}

#[tokio::main]
async fn main() -> Result<(), MakerError> {
    setup_logger();
    let cli = App::parse();

    match cli.command {
        Commands::Ping => {
            send_rpc_req(&RpcMsgReq::Ping).await?;
        }
        Commands::ContractUtxo => {
            send_rpc_req(&RpcMsgReq::ContractUtxo).await?;
        }
        Commands::ContractBalance => {
            send_rpc_req(&RpcMsgReq::ContractBalance).await?;
        }
        Commands::FidelityBalance => {
            send_rpc_req(&RpcMsgReq::FidelityBalance).await?;
        }
        Commands::FidelityUtxo => {
            send_rpc_req(&RpcMsgReq::FidelityUtxo).await?;
        }
        Commands::SeedBalance => {
            send_rpc_req(&RpcMsgReq::SeedBalance).await?;
        }
        Commands::SeedUtxo => {
            send_rpc_req(&RpcMsgReq::SeedUtxo).await?;
        }
        Commands::SwapBalance => {
            send_rpc_req(&RpcMsgReq::SwapBalance).await?;
        }
        Commands::SwapUtxo => {
            send_rpc_req(&RpcMsgReq::SwapUtxo).await?;
        }
    }

    Ok(())
}

async fn send_rpc_req(req: &RpcMsgReq) -> Result<(), MakerError> {
    let mut stream = TcpStream::connect("127.0.0.1:8080").await?;

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

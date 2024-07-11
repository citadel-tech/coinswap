use clap::Parser;


use coinswap::{
    market::rpc::{read_rpc_message, RpcMsgReq}, utill::{send_message, setup_logger}, maker::error::MakerError,
};


use serde::{Deserialize, Serialize};
use tokio::{io::BufReader, net::TcpStream};


#[derive(Serialize, Deserialize, Debug)]
enum Message {
    Hello,
}

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
    /// Sends a Ping
    Ping
}

#[tokio::main]
async fn main() -> Result<(), MakerError> {
    setup_logger();
    let cli = App::parse();

    match cli.command {
        Commands::Ping => {
            log::info!("Sending Ping");
            send_rpc_req(&RpcMsgReq::Ping).await?;
        }
    }

    Ok(())
}

async  fn send_rpc_req(req : &RpcMsgReq) -> Result<(), MakerError> {
    log::info!("Connecting to 127.0.0.1:4321");
    let mut stream = TcpStream::connect("127.0.0.1:4321").await?;
    println!("{:?}", stream);
    log::info!("Connected to 127.0.0.1:4321");

    let (read_half, mut write_half) = stream.split();

    if let Err(e) = send_message(&mut write_half, &req).await {
        log::error!("Error Sending RPC message : {:?}", e);
    };


    if let Some(rpc_resp) = read_rpc_message(&mut BufReader::new(read_half)).await? {
        log::info!("RPC response received: {:?}", rpc_resp);
    } else {
        log::error!("RPC response received: None");
    }

    Ok(())
}
use clap::Parser;

use coinswap::{
    maker::error::MakerError,
    market::rpc::{read_resp_message, RpcMsgReq, RpcMsgResp},
    utill::{send_message, setup_logger},
};

use tokio::{io::BufReader, net::TcpStream};

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

async fn send_rpc_req(req: &RpcMsgReq) -> Result<(), MakerError> {
    let mut stream = TcpStream::connect("127.0.0.1:4321").await?;
    println!("{:?}", stream);

    let (read_half, mut write_half) = stream.split();

    if let Err(e) = send_message(&mut write_half, &req).await {
        log::error!("Error Sending RPC message : {:?}", e);
    };

    if let Some(RpcMsgResp::ListAddressesResp(list)) =
        read_resp_message(&mut BufReader::new(read_half)).await?
    {
        println!("Maker Addresses: {:?}", list);
    } else {
        log::error!("RPC response received: None");
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), MakerError> {
    setup_logger();
    let cli = App::parse();

    match cli.command {
        Commands::ListAddresses => {
            send_rpc_req(&RpcMsgReq::ListAddresses).await?;
        }
    }

    Ok(())
}

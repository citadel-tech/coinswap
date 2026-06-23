use std::{net::TcpStream, path::PathBuf, time::Duration};

use clap::Parser;
use coinswap::{
    maker::{load_rpc_cookie, MakerError, RpcAuthEnvelope, RpcMsgReq, RpcMsgResp},
    utill::{get_maker_dir, read_message, send_message, MIN_FEE_RATE},
};

/// A simple command line app to operate the makerd server.
///
/// The app works as an RPC client for makerd, useful to access the server, retrieve information, and manage server operations.
///
/// For more detailed usage information, please refer: <https://github.com/citadel-tech/coinswap/blob/master/docs/maker-cli.md>
///
/// This is early beta, and there are known and unknown bugs. Please report issues at: <https://github.com/citadel-tech/coinswap/issues>
#[derive(Parser, Debug)]
#[command(version = option_env ! ("CARGO_PKG_VERSION").unwrap_or("unknown"),
author = option_env ! ("CARGO_PKG_AUTHORS").unwrap_or(""))]
struct App {
    /// Sets the rpc-port of Makerd
    #[arg(long, short = 'p', default_value = "127.0.0.1:6103")]
    rpc_port: String,
    /// Optional maker data directory (must match the running makerd instance)
    #[arg(long, short = 'd')]
    data_directory: Option<PathBuf>,
    /// The command to execute
    #[command(subcommand)]
    command: Commands,
}

#[derive(Parser, Debug)]
enum Commands {
    /// Sends a ping to makerd. Will return a pong.
    SendPing,
    /// Lists all utxos in the wallet. Including fidelity bonds.
    ListUtxo,
    /// Lists utxos received from incoming swaps.
    ListUtxoSwap,
    /// Lists HTLC contract utxos.
    ListUtxoContract,
    /// Lists fidelity bond utxos.
    ListUtxoFidelity,
    /// Get total wallet balances of different categories.
    /// regular: All single signature regular wallet coins (seed balance).
    /// swap: All 2of2 multisig coins received in swaps.
    /// contract: All live contract transaction balance locked in timelocks. If you see value in this field, you have unfinished or malfinished swaps. You can claim them back with the recover command.
    /// fidelity: All coins locked in fidelity bonds.
    /// spendable: Spendable amount in wallet (regular + swap balance).
    GetBalances,
    /// Gets a new bitcoin receiving address
    GetNewAddress,
    /// Send Bitcoin to an external address and return the txid.
    SendToAddress {
        /// Recipient's address.
        #[arg(long, short = 't')]
        address: String,
        /// Amount to send in sats
        #[arg(long, short = 'a')]
        amount: u64,
        /// Feerate in sats/vByte. Defaults to 2 sats/vByte
        #[arg(long, short = 'f')]
        feerate: Option<f64>,
    },
    /// Show the server tor address
    ShowTorAddress,
    /// Show the data directory path
    ShowDataDir,
    /// Shutdown the makerd server
    Stop,
    /// Show all the fidelity bonds, current and previous, with an (index, {bond_proof, is_spent}) tuple.
    ShowFidelity,
    /// Sync the Maker wallet with the current blockchain state.
    SyncWallet,
}

fn main() -> Result<(), MakerError> {
    let cli = App::parse();

    let data_dir = cli.data_directory.clone().unwrap_or_else(get_maker_dir);
    let token = load_rpc_cookie(&data_dir)?;

    let stream = TcpStream::connect(cli.rpc_port)?;

    match cli.command {
        Commands::SendPing => {
            send_rpc_req(stream, &token, RpcMsgReq::Ping)?;
        }
        Commands::ListUtxoContract => {
            send_rpc_req(stream, &token, RpcMsgReq::ContractUtxo)?;
        }
        Commands::ListUtxoFidelity => {
            send_rpc_req(stream, &token, RpcMsgReq::FidelityUtxo)?;
        }
        Commands::GetBalances => {
            send_rpc_req(stream, &token, RpcMsgReq::Balances)?;
        }
        Commands::ListUtxo => {
            send_rpc_req(stream, &token, RpcMsgReq::Utxo)?;
        }
        Commands::ListUtxoSwap => {
            send_rpc_req(stream, &token, RpcMsgReq::SwapUtxo)?;
        }
        Commands::GetNewAddress => {
            send_rpc_req(stream, &token, RpcMsgReq::NewAddress)?;
        }
        Commands::SendToAddress {
            address,
            amount,
            feerate,
        } => {
            send_rpc_req(
                stream,
                &token,
                RpcMsgReq::SendToAddress {
                    address,
                    amount,
                    feerate: feerate.unwrap_or(MIN_FEE_RATE),
                },
            )?;
        }
        Commands::ShowTorAddress => {
            send_rpc_req(stream, &token, RpcMsgReq::GetTorAddress)?;
        }
        Commands::ShowDataDir => {
            send_rpc_req(stream, &token, RpcMsgReq::GetDataDir)?;
        }
        Commands::Stop => {
            send_rpc_req(stream, &token, RpcMsgReq::Stop)?;
        }
        Commands::ShowFidelity => {
            send_rpc_req(stream, &token, RpcMsgReq::ListFidelity)?;
        }
        Commands::SyncWallet => {
            send_rpc_req(stream, &token, RpcMsgReq::SyncWallet)?;
        }
    }

    Ok(())
}

fn send_rpc_req(mut stream: TcpStream, token: &str, req: RpcMsgReq) -> Result<(), MakerError> {
    stream.set_write_timeout(Some(Duration::from_secs(20)))?;

    let envelope = RpcAuthEnvelope {
        token: token.to_string(),
        msg: req,
    };

    send_message(&mut stream, &envelope)?;

    let response_bytes = read_message(&mut stream)?;
    let response: RpcMsgResp = serde_cbor::from_slice(&response_bytes)?;

    if matches!(response, RpcMsgResp::Pong) {
        println!("success");
    } else {
        println!("{response}");
    }

    Ok(())
}

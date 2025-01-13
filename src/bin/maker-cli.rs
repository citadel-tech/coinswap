use std::{net::TcpStream, time::Duration};

use clap::Parser;
use coinswap::{
    maker::{MakerError, RpcMsgReq, RpcMsgResp},
    utill::{read_message, send_message, setup_maker_logger},
};
use bitcoin::Amount;

/// A simple command line app to operate the makerd server.
///
/// The app works as a RPC client for makerd, useful to access the server, retrieve information, and manage server operations.
///
/// For more detailed usage information, please refer: https://github.com/citadel-tech/coinswap/blob/master/docs/app%20demos/maker-cli.md
///
/// This is early beta, and there are known and unknown bugs. Please report issues at: https://github.com/citadel-tech/coinswap/issues
#[derive(Parser, Debug)]
#[clap(version = option_env ! ("CARGO_PKG_VERSION").unwrap_or("unknown"),
author = option_env ! ("CARGO_PKG_AUTHORS").unwrap_or(""))]
struct App {
    /// Sets the rpc-port of Makerd
    #[clap(long, short = 'p', default_value = "127.0.0.1:6103")]
    rpc_port: String,
    /// The command to execute
    #[clap(subcommand)]
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
    /// Get total wallet balance, excluding Fidelity bonds.
    GetBalance,
    /// Get total balance received via incoming swaps.
    GetBalanceSwap,
    /// Get total balances of HTLC contract utxos.
    GetBalanceContract,
    /// Get total amount locked in fidelity bonds.
    GetBalanceFidelity,
    /// Gets a new bitcoin receiving address
    GetNewAddress,
    /// Send Bitcoin to an external address and returns the txid.
    SendToAddress {
        /// Recipient's address.
        #[clap(long, short = 't')]
        address: String,
<<<<<<< HEAD
        amount: Amount,
        fee:Amount,
=======
        /// Amount to send in sats
        #[clap(long, short = 'a')]
        amount: u64,
        /// Total fee to be paid in sats
        #[clap(long, short = 'f')]
        fee: u64,
>>>>>>> 58a3446c6c5617d2f5761117ad2278068e215443
    },
    /// Show the server tor address
    ShowTorAddress,
    /// Show the data directory path
    ShowDataDir,
    /// Shutdown the makerd server
    Stop,
    /// Redeems the fidelity bond if timelock is matured. Returns the txid of the spending transaction.
    RedeemFidelity {
        #[clap(long, short = 'i', default_value = "0")]
        index: u32,
    },
    /// Show all the fidelity bonds, current and previous, with an (index, {bond_proof, is_spent}) tupple.
    ShowFidelity,
    /// Sync the maker wallet with current blockchain state.
    SyncWallet,
}

fn main() -> Result<(), MakerError> {
    setup_maker_logger(log::LevelFilter::Info);
    let cli = App::parse();

    let stream = TcpStream::connect(cli.rpc_port)?;

    match cli.command {
        Commands::SendPing => {
            send_rpc_req(stream, RpcMsgReq::Ping)?;
        }
        Commands::ListUtxoContract => {
            send_rpc_req(stream, RpcMsgReq::ContractUtxo)?;
        }
        Commands::GetBalanceContract => {
            send_rpc_req(stream, RpcMsgReq::ContractBalance)?;
        }
        Commands::GetBalanceFidelity => {
            send_rpc_req(stream, RpcMsgReq::FidelityBalance)?;
        }
        Commands::ListUtxoFidelity => {
            send_rpc_req(stream, RpcMsgReq::FidelityUtxo)?;
        }
        Commands::GetBalance => {
            send_rpc_req(stream, RpcMsgReq::Balance)?;
        }
        Commands::ListUtxo => {
            send_rpc_req(stream, RpcMsgReq::Utxo)?;
        }
        Commands::GetBalanceSwap => {
            send_rpc_req(stream, RpcMsgReq::SwapBalance)?;
        }
        Commands::ListUtxoSwap => {
            send_rpc_req(stream, RpcMsgReq::SwapUtxo)?;
        }
        Commands::GetNewAddress => {
            send_rpc_req(stream, RpcMsgReq::NewAddress)?;
        }
        Commands::SendToAddress {
            address,
            amount,
            fee,
        } => {
            send_rpc_req(
                stream,
                RpcMsgReq::SendToAddress {
                    address,
                    amount,
                    fee,
                },
            )?;
        }
        Commands::ShowTorAddress => {
            send_rpc_req(stream, RpcMsgReq::GetTorAddress)?;
        }
        Commands::ShowDataDir => {
            send_rpc_req(stream, RpcMsgReq::GetDataDir)?;
        }
        Commands::Stop => {
            send_rpc_req(stream, RpcMsgReq::Stop)?;
        }
        Commands::RedeemFidelity { index } => {
            send_rpc_req(stream, RpcMsgReq::RedeemFidelity(index))?;
        }
        Commands::ShowFidelity => {
            send_rpc_req(stream, RpcMsgReq::ListFidelity)?;
        }
        Commands::SyncWallet => {
            send_rpc_req(stream, RpcMsgReq::SyncWallet)?;
        }
    }

    Ok(())
}

fn send_rpc_req(mut stream: TcpStream, req: RpcMsgReq) -> Result<(), MakerError> {
    // stream.set_read_timeout(Some(Duration::from_secs(20)))?;
    stream.set_write_timeout(Some(Duration::from_secs(20)))?;

    send_message(&mut stream, &req)?;

    let response_bytes = read_message(&mut stream)?;
    let response: RpcMsgResp = serde_cbor::from_slice(&response_bytes)?;

    if matches!(response, RpcMsgResp::Pong) {
        println!("success");
    } else {
        println!("{}", response);
    }

    Ok(())
}

use std::{net::TcpStream, time::Duration};

use clap::Parser;
use coinswap::{
    maker::{MakerError, RpcMsgReq, RpcMsgResp},
    utill::{read_message, send_message, MIN_FEE_RATE},
};

/// A simple command line app to operate the makerd server.
///
/// The app works as an RPC client for makerd, useful to access the server, retrieve information, and manage server operations.
///
/// For more detailed usage information, please refer: <https://github.com/citadel-tech/coinswap/blob/master/docs/app%20demos/maker-cli.md>
///
/// This is early beta, and there are known and unknown bugs. Please report issues at: <https://github.com/citadel-tech/coinswap/issues>
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
        #[clap(long, short = 't')]
        address: String,
        /// Amount to send in sats
        #[clap(long, short = 'a')]
        amount: u64,
        /// Feerate in sats/vByte. Defaults to 2 sats/vByte
        #[clap(long, short = 'f')]
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
    /// Sync the maker wallet with the current blockchain state.
    SyncWallet,
}

fn main() -> Result<(), MakerError> {
    let cli = App::parse();

    #[cfg(debug_assertions)]
    log::debug!("[MAKER_CLI] Connecting to RPC at: {}", cli.rpc_port);

    let stream = TcpStream::connect(&cli.rpc_port)?;

    #[cfg(debug_assertions)]
    log::debug!("[MAKER_CLI] Connected successfully");

    match cli.command {
        Commands::SendPing => {
            #[cfg(debug_assertions)]
            log::debug!("[MAKER_CLI] Sending Ping command");

            send_rpc_req(stream, RpcMsgReq::Ping)?;
        }
        Commands::ListUtxoContract => {
            #[cfg(debug_assertions)]
            log::debug!("[MAKER_CLI] Sending ContractUtxo command");

            send_rpc_req(stream, RpcMsgReq::ContractUtxo)?;
        }
        Commands::ListUtxoFidelity => {
            #[cfg(debug_assertions)]
            log::debug!("[MAKER_CLI] Sending FidelityUtxo command");

            send_rpc_req(stream, RpcMsgReq::FidelityUtxo)?;
        }
        Commands::GetBalances => {
            #[cfg(debug_assertions)]
            log::debug!("[MAKER_CLI] Sending Balances command");

            send_rpc_req(stream, RpcMsgReq::Balances)?;
        }
        Commands::ListUtxo => {
            #[cfg(debug_assertions)]
            log::debug!("[MAKER_CLI] Sending Utxo command");

            send_rpc_req(stream, RpcMsgReq::Utxo)?;
        }
        Commands::ListUtxoSwap => {
            #[cfg(debug_assertions)]
            log::debug!("[MAKER_CLI] Sending SwapUtxo command");

            send_rpc_req(stream, RpcMsgReq::SwapUtxo)?;
        }
        Commands::GetNewAddress => {
            #[cfg(debug_assertions)]
            log::debug!("[MAKER_CLI] Sending NewAddress command");

            send_rpc_req(stream, RpcMsgReq::NewAddress)?;
        }
        Commands::SendToAddress {
            address,
            amount,
            feerate,
        } => {
            #[cfg(debug_assertions)]
            log::debug!(
                "[MAKER_CLI] Sending SendToAddress command | address={} | amount={} | feerate={:?}",
                address,
                amount,
                feerate
            );

            send_rpc_req(
                stream,
                RpcMsgReq::SendToAddress {
                    address,
                    amount,
                    feerate: feerate.unwrap_or(MIN_FEE_RATE),
                },
            )?;
        }
        Commands::ShowTorAddress => {
            #[cfg(debug_assertions)]
            log::debug!("[MAKER_CLI] Sending GetTorAddress command");

            send_rpc_req(stream, RpcMsgReq::GetTorAddress)?;
        }
        Commands::ShowDataDir => {
            #[cfg(debug_assertions)]
            log::debug!("[MAKER_CLI] Sending GetDataDir command");

            send_rpc_req(stream, RpcMsgReq::GetDataDir)?;
        }
        Commands::Stop => {
            #[cfg(debug_assertions)]
            log::debug!("[MAKER_CLI] Sending Stop command");

            send_rpc_req(stream, RpcMsgReq::Stop)?;
        }
        Commands::ShowFidelity => {
            #[cfg(debug_assertions)]
            log::debug!("[MAKER_CLI] Sending ListFidelity command");

            send_rpc_req(stream, RpcMsgReq::ListFidelity)?;
        }
        Commands::SyncWallet => {
            #[cfg(debug_assertions)]
            log::debug!("[MAKER_CLI] Sending SyncWallet command");

            send_rpc_req(stream, RpcMsgReq::SyncWallet)?;
        }
    }

    #[cfg(debug_assertions)]
    log::debug!("[MAKER_CLI] Command execution completed");

    Ok(())
}

fn send_rpc_req(mut stream: TcpStream, req: RpcMsgReq) -> Result<(), MakerError> {
    stream.set_write_timeout(Some(Duration::from_secs(20)))?;

    #[cfg(debug_assertions)]
    log::debug!("[MAKER_CLI_RPC] Sending request: {:?}", req);

    send_message(&mut stream, &req)?;

    #[cfg(debug_assertions)]
    log::debug!("[MAKER_CLI_RPC] Request sent, waiting for response");

    let response_bytes = read_message(&mut stream)?;
    let response: RpcMsgResp = serde_cbor::from_slice(&response_bytes)?;

    #[cfg(debug_assertions)]
    log::debug!("[MAKER_CLI_RPC] Received response: {:?}", response);

    if matches!(response, RpcMsgResp::Pong) {
        println!("success");
    } else {
        println!("{response}");
    }

    Ok(())
}

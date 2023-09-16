use bitcoin::hashes::hash160::Hash as Hash160;
use clap::{Parser, Subcommand};
use std::{
    path::PathBuf,
    sync::{Arc, RwLock},
};

use coinswap::{
    error::TeleportError,
    maker::MakerBehavior,
    scripts::{
        maker::run_maker,
        market::download_and_display_offers,
        recovery::recover_from_incomplete_coinswap,
        setup_logger,
        taker::run_taker,
        wallet::{
            direct_send, display_wallet_addresses, display_wallet_balance, generate_wallet,
            print_fidelity_bond_address, print_receive_invoice, recover_wallet,
        },
    },
    wallet::{
        fidelity::YearAndMonth, CoinToSpend, Destination, DisplayAddressType, RPCConfig,
        SendAmount, WalletMode,
    },
};

#[derive(Parser, Debug)]
#[command(author, version, about)]
#[command(next_line_help = true)]
struct ArgsWithWalletFile {
    /// Wallet file Name
    #[arg(long, short, default_value = "wallet.teleport", value_parser = clap::value_parser!(PathBuf))]
    wallet_file_name: PathBuf,

    /// Dont broadcast transactions, only output their transaction hex string
    /// Only for commands which involve sending transactions e.g. recover-from-incomplete-coinswap
    #[arg(long, short, default_value_t = true)]
    dont_broadcast: bool,

    /// Miner fee rate, in satoshis per thousand vbytes, i.e. 1000 = 1 sat/vb
    #[arg( long, short, default_value_t = 1000, value_parser = clap::value_parser!(u64).range(1..))]
    fee_rate: u64,

    /// Subcommand
    #[clap(subcommand)]
    subcommand: WalletArgsSubcommand,
}

#[derive(Subcommand, Debug)]
enum WalletArgsSubcommand {
    /// Generates a new seed phrase and wallet file
    GenerateWallet,

    /// Recovers a wallet file from an existing seed phrase
    RecoverWallet,

    /// Prints current wallet balance.
    WalletBalance {
        /// Whether to print entire TXIDs and addresses
        #[arg(long, short, default_value_t = false)]
        long_form: bool,
    },

    /// Dumps all addresses in wallet file, only useful for debugging
    DisplayWalletAddresses {
        /// Address types: "all", "masterkey", "seed", "incomingswap", "outgoingswap",
        /// "swap", "incomingcontract", "outgoingcontract", "contract", "fidelitybond".
        /// Default is "all"
        #[arg(long, short, value_enum, default_value = "All")]
        types: DisplayAddressType,
    },

    /// Prints receive invoice.
    GetReceiveInvoice,

    /// Runs yield generator aiming to produce an income
    RunYieldGenerator {
        /// Port to listen on, default is 6102
        #[arg(long, short, default_value_t = 6102)]
        port: u16,
        /// Special behavior used for testing e.g. "closeonsignsenderscontracttx"
        /// TODO more information on usefulness
        #[arg(long, short)]
        special_behavior: Option<String>,
    },

    /// Prints a fidelity bond timelocked address
    GetFidelityBondAddress {
        /// Locktime value of timelocked address as yyyy-mm year and month, for example "2025-03"
        #[arg( long, short, value_parser = clap::value_parser!(YearAndMonth),
    )]
        year_and_month: YearAndMonth,
    },

    /// Runs Taker.
    DoCoinswap {
        /// Amount to send (in sats)
        #[arg(long, short, default_value_t = 50000)]
        send_amount: u64, //TODO convert this to SendAmount

        /// How many makers to route through, default 2
        #[arg(long, short, default_value_t = 2, value_parser = clap::value_parser!(u16).range(1..))]
        maker_count: u16,

        /// How many transactions per hop, default 3
        #[arg(long, short, default_value_t = 3, value_parser = clap::value_parser!(u16).range(1..) )]
        tx_count: u32,
    },

    /// Broadcast contract transactions for incomplete coinswap. Locked up bitcoins are
    /// returned to your wallet after the timeout
    RecoverFromIncompleteCoinswap {
        /// Hashvalue as hex string which uniquely identifies the coinswap
        #[arg(long)]
        hashvalue: Hash160,
    },

    /// Download all offers from all makers out there. If bitcoin node not configured then
    /// provide the network as an argument, can also optionally download from one given maker
    DownloadOffers {
        /// Network in question, options are "main", "test", "signet". Only used if configured
        /// bitcoin node RPC is unreachable
        #[arg(long, short)]
        network: Option<String>,
        /// Optional single maker address to only download from. Useful if testing if your own
        /// maker is reachable
        #[arg(long, short)]
        maker_address: Option<String>,
    },

    /// Send a transaction from the wallet
    DirectSend {
        /// Amount to send (in sats), or "max" for fully-spending with no change
        #[arg(long, short, value_enum)]
        send_amount: SendAmount,

        /// Address to send coins to, or "wallet" to send back to own wallet
        #[arg(long, short, value_enum, default_value = "wallet")]
        destination: Destination,

        /// Coins to spend as inputs, either in long form "<txid>:vout" or short
        /// form "txid-prefix..txid-suffix:vout"
        #[arg(long, short, value_enum)]
        coins_to_spend: Vec<CoinToSpend>,
    },
}

fn main() -> Result<(), TeleportError> {
    setup_logger();
    let args = ArgsWithWalletFile::parse();
    // let args = ArgsWithWalletFile::from_args();

    match args.subcommand {
        WalletArgsSubcommand::GenerateWallet => {
            generate_wallet(&args.wallet_file_name, None)?;
        }
        WalletArgsSubcommand::RecoverWallet => {
            recover_wallet(&args.wallet_file_name)?;
        }
        WalletArgsSubcommand::WalletBalance { long_form } => {
            display_wallet_balance(&args.wallet_file_name, None, Some(long_form))?;
        }
        WalletArgsSubcommand::DisplayWalletAddresses { types } => {
            display_wallet_addresses(&args.wallet_file_name, types)?;
        }
        WalletArgsSubcommand::GetReceiveInvoice => {
            print_receive_invoice(&args.wallet_file_name)?;
        }
        WalletArgsSubcommand::RunYieldGenerator {
            port,
            special_behavior,
        } => {
            let maker_special_behavior = match special_behavior.unwrap_or(String::new()).as_str() {
                "closeonsignsenderscontracttx" => MakerBehavior::CloseOnSignSendersContractTx,
                _ => MakerBehavior::Normal,
            };
            run_maker(
                &args.wallet_file_name,
                &RPCConfig::default(),
                port,
                Some(WalletMode::Testing),
                maker_special_behavior,
                Arc::new(RwLock::new(false)),
            )?;
        }
        WalletArgsSubcommand::GetFidelityBondAddress { year_and_month } => {
            print_fidelity_bond_address(&args.wallet_file_name, &year_and_month)?;
        }
        WalletArgsSubcommand::DoCoinswap {
            send_amount,
            maker_count,
            tx_count,
        } => {
            run_taker(
                &args.wallet_file_name,
                Some(WalletMode::Testing),
                None,
                args.fee_rate,
                send_amount,
                maker_count,
                tx_count,
                None,
            );
        }
        WalletArgsSubcommand::RecoverFromIncompleteCoinswap { hashvalue } => {
            recover_from_incomplete_coinswap(
                &args.wallet_file_name,
                hashvalue,
                args.dont_broadcast,
            )?;
        }
        WalletArgsSubcommand::DownloadOffers {
            network,
            maker_address,
        } => {
            download_and_display_offers(network, maker_address);
        }
        WalletArgsSubcommand::DirectSend {
            send_amount,
            destination,
            coins_to_spend,
        } => {
            direct_send(
                &args.wallet_file_name,
                args.fee_rate,
                send_amount,
                destination,
                &coins_to_spend,
                args.dont_broadcast,
            )?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod clap_parser_test {
    use crate::ArgsWithWalletFile;

    #[test]
    fn verify_clap_cli_test() {
        use clap::CommandFactory;
        ArgsWithWalletFile::command().debug_assert()
    }
}

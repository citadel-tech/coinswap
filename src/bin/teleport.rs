use bitcoin::{
    consensus::encode::deserialize,
    hashes::{hash160::Hash as Hash160, hex::FromHex},
    Script, Transaction,
};

use std::path::{Path, PathBuf};
use structopt::StructOpt;

use teleport::{
    error::TeleportError,
    maker::server::MakerBehavior,
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
        watchtower::run_watchtower,
    },
    wallet::{
        fidelity::YearAndMonth, CoinToSpend, Destination, DisplayAddressType, SendAmount,
        WalletMode,
    },
    watchtower::{
        client::test_watchtower_client,
        routines::{ContractTransaction, ContractsInfo},
    },
};

#[derive(Debug, StructOpt)]
#[structopt(name = "teleport", about = "A tool for CoinSwap")]
struct ArgsWithWalletFile {
    /// Wallet file
    #[structopt(
        default_value = "wallet.teleport",
        parse(from_os_str),
        long,
        short = "w"
    )]
    wallet_file_name: PathBuf,

    /// Dont broadcast transactions, only output their transaction hex string
    /// Only for commands which involve sending transactions e.g. recover-from-incomplete-coinswap
    #[structopt(short, long)]
    dont_broadcast: bool,

    /// Miner fee rate, in satoshis per thousand vbytes, i.e. 1000 = 1 sat/vb
    #[structopt(default_value = "1000", short = "f", long)]
    fee_rate: u64,

    /// Subcommand
    #[structopt(flatten)]
    subcommand: Subcommand,
}

#[derive(Debug, StructOpt)]
#[structopt(name = "teleport", about = "A tool for CoinSwap")]
enum Subcommand {
    /// Generates a new seed phrase and wallet file
    GenerateWallet,

    /// Recovers a wallet file from an existing seed phrase
    RecoverWallet,

    /// Prints current wallet balance.
    WalletBalance {
        /// Whether to print entire TXIDs and addresses
        long_form: Option<bool>,
    },

    /// Dumps all addresses in wallet file, only useful for debugging
    DisplayWalletAddresses {
        /// Address types: "all", "masterkey", "seed", "incomingswap", "outgoingswap",
        /// "swap", "incomingcontract", "outgoingcontract", "contract", "fidelitybond".
        /// Default is "all"
        types: Option<DisplayAddressType>,
    },

    /// Prints receive invoice.
    GetReceiveInvoice,

    /// Runs yield generator aiming to produce an income
    RunYieldGenerator {
        /// Port to listen on, default is 6102
        port: Option<u16>,
        /// Special behavior used for testing e.g. "closeonsignsenderscontracttx"
        special_behavior: Option<String>,
    },

    /// Prints a fidelity bond timelocked address
    GetFidelityBondAddress {
        /// Locktime value of timelocked address as yyyy-mm year and month, for example "2025-03"
        year_and_month: YearAndMonth,
    },

    /// Runs Taker.
    DoCoinswap {
        /// Amount to send (in sats)
        send_amount: u64, //TODO convert this to SendAmount
        /// How many makers to route through, default 2
        maker_count: Option<u16>,
        /// How many transactions per hop, default 3
        tx_count: Option<u32>,
    },

    /// Broadcast contract transactions for incomplete coinswap. Locked up bitcoins are
    /// returned to your wallet after the timeout
    RecoverFromIncompleteCoinswap {
        /// Hashvalue as hex string which uniquely identifies the coinswap
        hashvalue: Hash160,
    },

    /// Download all offers from all makers out there. If bitcoin node not configured then
    /// provide the network as an argument, can also optionally download from one given maker
    DownloadOffers {
        /// Network in question, options are "main", "test", "signet". Only used if configured
        /// bitcoin node RPC is unreachable
        network: Option<String>,
        /// Optional single maker address to only download from. Useful if testing if your own
        /// maker is reachable
        maker_address: Option<String>,
    },

    /// Send a transaction from the wallet
    DirectSend {
        /// Amount to send (in sats), or "max" for fully-spending with no change
        send_amount: SendAmount,
        /// Address to send coins to, or "wallet" to send back to own wallet
        destination: Destination,
        /// Coins to spend as inputs, either in long form "<txid>:vout" or short
        /// form "txid-prefix..txid-suffix:vout"
        coins_to_spend: Vec<CoinToSpend>,
    },

    /// Run watchtower
    RunWatchtower {
        /// File path used for the watchtower data file, default "watchtower.dat"
        data_file_path: Option<PathBuf>,
    },

    /// Test watchtower client
    TestWatchtowerClient {
        contract_transactions_hex: Vec<String>,
    },
}

fn main() -> Result<(), TeleportError> {
    setup_logger();
    let args = ArgsWithWalletFile::from_args();

    match args.subcommand {
        Subcommand::GenerateWallet => {
            generate_wallet(&args.wallet_file_name, None)?;
        }
        Subcommand::RecoverWallet => {
            recover_wallet(&args.wallet_file_name)?;
        }
        Subcommand::WalletBalance { long_form } => {
            display_wallet_balance(&args.wallet_file_name, None, long_form)?;
        }
        Subcommand::DisplayWalletAddresses { types } => {
            display_wallet_addresses(
                &args.wallet_file_name,
                types.unwrap_or(DisplayAddressType::All),
            )?;
        }
        Subcommand::GetReceiveInvoice => {
            print_receive_invoice(&args.wallet_file_name)?;
        }
        Subcommand::RunYieldGenerator {
            port,
            special_behavior,
        } => {
            let maker_special_behavior = match special_behavior.unwrap_or(String::new()).as_str() {
                "closeonsignsenderscontracttx" => MakerBehavior::CloseOnSignSendersContractTx,
                _ => MakerBehavior::Normal,
            };
            run_maker(
                &args.wallet_file_name,
                port.unwrap_or(6102),
                Some(WalletMode::Testing),
                maker_special_behavior,
                None,
            )?;
        }
        Subcommand::GetFidelityBondAddress { year_and_month } => {
            print_fidelity_bond_address(&args.wallet_file_name, &year_and_month)?;
        }
        Subcommand::DoCoinswap {
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
                maker_count.unwrap_or(2),
                tx_count.unwrap_or(3),
            );
        }
        Subcommand::RecoverFromIncompleteCoinswap { hashvalue } => {
            recover_from_incomplete_coinswap(
                &args.wallet_file_name,
                hashvalue,
                args.dont_broadcast,
            )?;
        }
        Subcommand::DownloadOffers {
            network,
            maker_address,
        } => {
            download_and_display_offers(network, maker_address);
        }
        Subcommand::DirectSend {
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
        Subcommand::RunWatchtower { data_file_path } => {
            run_watchtower(
                &data_file_path.unwrap_or(Path::new("watchtower.dat").to_path_buf()),
                None,
            )?;
        }
        Subcommand::TestWatchtowerClient {
            mut contract_transactions_hex,
        } => {
            if contract_transactions_hex.is_empty() {
                // https://bitcoin.stackexchange.com/questions/68811/what-is-the-absolute-smallest-size-of-the-data-bytes-that-a-blockchain-transac
                contract_transactions_hex =
                    vec![String::from(concat!("020000000001010000000000000",
                "0000000000000000000000000000000000000000000000000000000000000fdffffff010100000000",
                "000000160014ffffffffffffffffffffffffffffffffffffffff02210200000000000000000000000",
                "000000000000000000000000000000000000000014730440220777777777777777777777777777777",
                "777777777777777777777777777777777702205555555555555555555555555555555555555555555",
                "5555555555555555555550100000000"))];
            }
            let contract_txes = contract_transactions_hex
                .iter()
                .map(|cth| ContractTransaction {
                    tx: deserialize::<Transaction>(
                        &Vec::from_hex(&cth).expect("Invalid transaction hex string"),
                    )
                    .expect("Unable to deserialize transaction hex"),
                    redeemscript: Script::new(),
                    hashlock_spend_without_preimage: None,
                    timelock_spend: None,
                    timelock_spend_broadcasted: false,
                })
                .collect::<Vec<ContractTransaction>>();
            test_watchtower_client(ContractsInfo {
                contract_txes,
                wallet_label: String::new(),
            });
        }
    }

    Ok(())
}

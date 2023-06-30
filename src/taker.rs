/* Staging Area of Conceptual Taker struct */
/* Objective: simplify interface and interactions. */
/* The current pass around mutable variables many of which are security critical */
/* Proper encapsulation requires proper structuring of all the state variables in taker-protocol.rs */

use std::path::PathBuf;

use bitcoincore_rpc::Client;
use tokio::net::ToSocketAddrs;

use crate::{wallet_sync::{OutgoingSwapCoin, IncomingSwapCoin, Wallet}, contracts::WatchOnlySwapCoin, offerbook_sync::MakerAddress, taker_protocol::SwapParams, messages::Preimage};

// Taker's internal OfferBook.
// This should be updatable from public offer servers.
struct OfferBook {
    /* placeholder */
}

struct TakerConfig {
    /* placeholder */
    /* This should include the hard coded config variables above */
}

impl TakerConfig {
    fn read_from_file(config_file: &PathBuf) -> Self {
        unimplemented!();
    }
}

// /// Represents the active set of Swapcoins for a coinswap round.
// struct ActiveSwapCoins{
//     pub outgoing_swapcoins: Vec<OutgoingSwapCoin>,
//     pub watchonly_swapcoins: Vec<Vec<WatchOnlySwapCoin>>,
//     pub incoming_swapcoins: Vec<IncomingSwapCoin>
// }

// impl ActiveSwapCoins {
//     fn get_incomings(&self) -> &Vec<IncomingSwapCoin> {
//         &self.incoming_swapcoins
//     }

//     fn get_outgoings(&self) -> &Vec<OutgoingSwapCoin> {
//         &self.outgoing_swapcoins
//     }

//     fn get_watchonlys(&self) -> &Vec<Vec<WatchOnlySwapCoin>> {
//         &self.watchonly_swapcoins
//     }

//     fn set_incoming_swapcoins(&mut self, swapcoins: &Vec<IncomingSwapCoin>) {
//         // TODO: assert that swapcoins doesn't exist
//         self.incoming_swapcoins = swapcoins.clone();
//     }

//     fn add_outgoing_swapcoin(&mut self, swapcoins: &Vec<OutgoingSwapCoin>) {
//         // TODO: assert that swapcoins doesn't exist
//         self.outgoing_swapcoins = *swapcoins.clone();
//     }

//     fn add_watchonly_swapcoins(&mut self, swapcoins: &Vec<WatchOnlySwapCoin>) {
//         // TODO: assert that swapcoins doesn't exist
//         self.watchonly_swapcoins.push(*swapcoins)
//     }
// }

pub struct CurrentSwapInfo {
    pub outgoing_swapcoins: Vec<OutgoingSwapCoin>,
    pub watchonly_swapcoins: Vec<Vec<WatchOnlySwapCoin>>,
    pub incoming_swapcoins: Vec<IncomingSwapCoin>,
    // List of active makers for a coinswap round
    pub active_makers: Vec<MakerAddress>,
    /// The preimage for a active coinswap round
    pub active_preimage: Preimage
}

struct Taker {
    // Wallet should include rpc client inside.
    wallet: Wallet,
    // TODO: Move this inside Wallet
    rpc: Client,
    config: TakerConfig,
    offerbook: OfferBook,
    // All information regarding the current Swap
    current_swap_info: CurrentSwapInfo
}

impl Taker {
    // initialize a taker with given config and wallet.
    fn init(config_file: &PathBuf, wallet_file: &PathBuf) -> Self {
        unimplemented!();
    }

    // Update the internal offer book from a list of known public servers.
    fn offer_book_update(directory_server: impl ToSocketAddrs) {
        unimplemented!();
    }

    // Given a list of swap parameters try to complete a coinswap.
    // It should use the internal offer book and try to find the best set of makers to
    // swap with, satisfying the swap_params.
    fn coinswap(swap_params: SwapParams) {
        unimplemented!();
    }
}

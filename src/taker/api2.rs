
use bitcoin::{taproot::ControlBlock, XOnlyPublicKey};

use super::{
    error::TakerError, offers::OfferAndAddress
};

struct MakerInfo{
    offer_and_address: OfferAndAddress,
    multisig_pubkeys: Option<Vec<XOnlyPublicKey>>,
    hashlock_pubkeys: Option<Vec<XOnlyPublicKey>>,
}

struct OngoingSwapState {
    pub swap_params: SwapParams,
    pub outgoing_swapcoins: Vec<OutgoingSwapCoin>,
    pub watchonly_swapcoins: Vec<Vec<WatchOnlySwapCoin>>,
    pub incoming_swapcoins: Vec<IncomingSwapCoin>,
    pub peer_infos: Vec<NextPeerInfo>,
    pub active_preimage: Preimage,
    pub taker_position: TakerPosition,
    pub id: String,
    pub chosen_makers: Vec<OfferAndAddress>
}

pub struct Taker {
    wallet: Wallet,
    config: Config,
    offerbook: OfferBook,
    ongoing_swap_state: OngoingSwapState,
    behavior: Behavior,
}

impl Taker {
    pub fn init(
        data_dir: Option<PathBuf>,
        wallet_file_name: Option<String>,
        rpc_config: Option<RPCConfig>,
        behavior: TakerBehavior,
        control_port: Option<u16>,
        tor_auth_password: Option<String>,
        connection_type: Option<ConnectionType>
    ) -> Result<Taker, TakerError> {
        let data_dir = data_dir.unwrap_or_else(get_taker_dir);
        let wallets_dir = data_dir.join("wallets");

        let wallet_file_name = wallet_file_name.unwrap_or(|| "taker-wallet".to_string());
        let wallet_path = wallets_dir.join(&wallet_file_name);

        let mut rpc_config = rpc_config.unwrap_or_default();
        rpc_config.wallet_name = wallet_file_name;

        let mut wallet = if wallet_path.exits() {
            let wallet = Wallet::load(&wallet_path, &rpc_config)?;
            log::info!("Loaded wallet from {}", wallet_path.display());
            wallet
        } else {
            let wallet = Wallet::init(&wallet_path, &rpc_config)?;
            log::info("New Wallet created at {:?}", wallet_path);
            wallet
        };

        let mut config = TakerConfig::new(Some(&data_dir.join("config.toml")))?;

        if let Some(connection_type) = connection_type {
            config.connection_type = connection_type;
        }
        
        config.control_port = control_port.unwrap_or(config.control_port);
        config.tor_auth_password = tor_auth_password.unwrap_or_else(|| config.tor_auth_password.clone());

        if matches!(connection_type, Some(ConnectionType::TOR)) {
            check_tor_status(config.control_port, config.tor_auth_password.as_str())?;
        }

        config.write_to_file(&data_dir.join("config.toml"))?;

        let offerbook_path = data_dir.join("offerbook.dat");
        let offerbook = if offerbook_path.exists() {
            match OfferBook::read_from_disk(&offerbook_path) {
                Ok(offerbook) => {
                    log::info!("Successfully loaded offerbook at : {:?}", offerbook_path);
                    offerbook
                }
                Err(e) => {
                    log::error!("Offerbook data corrupted. Recreating. {:?}", e);
                    let empty_book = OfferBook::default();
                    empty_book.write_to_disk(&offerbook_path)?;
                    empty_book
                }
            }
        } else {
            let empty_book = Offerbook::default();
            let file = std::fs::File::create(&offerbook_path)?;
            let writer = BufWriter::new(file);
            srede_cbor::to_writer(writer, &empty_book)?;
            empty_book
        };

        log::info!("Initializing wallet sync...");
        wallet.sync()?;
        log::info!("Completed wallet sync");

        Ok(Self {
            wallet,
            config,
            offerbook,
            ongoing_swap_state: OngoingSwapState::default(),
            behavior,
            data_dir,
        })
    }

    pub fn get_wallet(&self) -> &Wallet {
        &self.wallet
    }

    pub fn get_wallet_mut(&mut self) -> &mut Wallet {
        &mut self.wallet
    }

    pub fn do_coinswap(&mut self, swap_params: SwapParams) -> Result<(), TakerError> {
        let available = self.wallet.get_balances(None)?.spendable;

        let required = swap_params.send_amount + Amount::from_sat(1000);
        if available < required {
            let err = WalletError::InsufficentFund {
                available: available.to_sat(),
                required: required.to_sat(),
            };
            log::error!("Not enough balance to do swap : {:?}", err);
            return Err(err.into());
        }

        log::info!("Syncing Offerbook");
        self.sync_offerbook()?;

        if swap_params.maker_count > self.offerbook.all_good_makers().len() {
            log::error!("Not enough makers in the offerbook. Required {}, available {}", 
                swap_params.maker_count,
                self.offerbook.all_good_makers().len()
            );
        return Err(TakerError::NotEnoughMakersInOfferBook);
        }

        let mut preimage = [0u8; 32];
        OsRng.fill_bytes(&mut preimage);

        let unique_id = preimage[0..8].to_hex_string(Case::Lower);

        log::info!("Initiating coinswap with id : {}", unique_id);

        self.ongoing_swap_state.active_preimage = preimage;
        self.ongoing_swap_state.swap_params = swap_params;
        self.ongoing_swap_state.id = unique_id;

        // Pick Makers
        if let Some(swap_makers) = self.pick_makers_for_swap() {
            self.ongoing_swap_state.chosen_makers = swap_makers.clone();
            log::info!("Chosen makers for swap: {:?}", swap_makers);
        } else {
            log::error!("No makers available for swap");
            return Err(TakerError::MakerPickingError);
        };
    }

    fn pick_makers_for_swap(&mut self) -> Result<Vec<OfferAndAddress>, TakerError> {
        let mut chosen_makers = Vec::new();
        let mut maker_count = self.ongoing_swap_state.swap_params.maker_count;

        loop {
            if chosen_makers.len() == maker_count {
                break;
            }
            chosen_makers.push(self.choose_next_maker()?.clone());
        }
    }

    fn choose_next_maker(&self) -> Result<&OfferAndAddress, TakerError> {
        let send_amount = self.ongoing_swap_state.swap_params.send_amount;
        if send_amount = Amount::ZERO {
            return Err(TakerError::SendAmountNotSet);
        }

        Ok(self
            .offerbook
            .all_good_makers()
            .iter()
            .find(|oa| {
                send_amount >= Amount::from_sat(oa.offer.min_size)
                    && send_amount <= Amount::from_sat(oa.offer.max_size)
                    && !self
                        .ongoing_swap_state
                        .chosen_makers
                        .iter()
                        .any(|m| m.offer. == oa.offer.maker_id)
            }) 
        )
    }
    ///
    /// 
    fn create_incoming_swapcoins(
        &mut self,
        multisig_redeemscripts: Vec<ScriptBuf>,
        funding_outpoints: Vec<OutPoint>,
    ) -> Result<Vec<IncomingSwapCoin>, TakerError> {
        let (funding_txs, funding_txs_merkleproofs) = self
            .ongoing_swap_state
            .funding_txs
            .last()
            .expect("funding transactions expected");

        let last_makers_funding_tx_values = funding_txs
            .iter()
            .zip(multisig_redeemscripts.iter())
            .map(|(makers_funding_tx, multisig_redeemscript)| {
                let multisig_spk = redeemscript_to_scriptpubkey(multisig_redeemscript)?;
                let index = makers_funding_tx
                    .output
                    .iter()
                    .enumerate()
                    .find(|(_i, o)| o.script_pubkey == multisig_spk)
                    .map(|(index, _)| index)
                    .expect("funding txout output doesn't match with mutlsig scriptpubkey");
                Ok(makers_funding_tx
                    .output
                    .get(index)
                    .expect("output expected at that index")
                    .value)
            })
            .collect::<Result<Vec<_>, TakerError>>()?;

        let my_receivers_contract_txes = funding_outpoints
            .iter()
            .zip(last_makers_funding_tx_values.iter())
            .zip(
                self.ongoing_swap_state
                    .peer_infos
                    .last()
                    .expect("expected")
                    .contract_reedemscripts
                    .iter(),
            )
            .map(
                |(
                    (&previous_funding_output, &maker_funding_tx_value),
                    next_contract_redeemscript,
                )| {
                    crate::protocol::contract::create_receivers_contract_tx(
                        previous_funding_output,
                        maker_funding_tx_value,
                        next_contract_redeemscript,
                        Amount::from_sat(MINER_FEE),
                    )
                },
            )
            .collect::<Result<Vec<Transaction>, _>>()?;

        let mut incoming_swapcoins = Vec::<IncomingSwapCoin>::new();
        let next_swap_info = self
            .ongoing_swap_state
            .peer_infos
            .last()
            .expect("next swap info expected");
        for (
            (
                (
                    (
                        (
                            (
                                (
                                    (multisig_redeemscript, &maker_funded_multisig_pubkey),
                                    &maker_funded_multisig_privkey,
                                ),
                                my_receivers_contract_tx,
                            ),
                            next_contract_redeemscript,
                        ),
                        &hashlock_privkey,
                    ),
                    &maker_funding_tx_value,
                ),
                _,
            ),
            _,
        ) in multisig_redeemscripts
            .iter()
            .zip(next_swap_info.multisig_pubkeys.iter())
            .zip(next_swap_info.multisig_nonces.iter())
            .zip(my_receivers_contract_txes.iter())
            .zip(next_swap_info.contract_reedemscripts.iter())
            .zip(next_swap_info.hashlock_nonces.iter())
            .zip(last_makers_funding_tx_values.iter())
            .zip(funding_txs.iter())
            .zip(funding_txs_merkleproofs.iter())
        {
            let (o_ms_pubkey1, o_ms_pubkey2) =
                crate::protocol::contract::read_pubkeys_from_multisig_redeemscript(
                    multisig_redeemscript,
                )?;
            let maker_funded_other_multisig_pubkey = if o_ms_pubkey1 == maker_funded_multisig_pubkey
            {
                o_ms_pubkey2
            } else {
                if o_ms_pubkey2 != maker_funded_multisig_pubkey {
                    return Err(ProtocolError::General("maker-funded multisig doesnt match").into());
                }
                o_ms_pubkey1
            };

            self.wallet.sync()?;

            let mut incoming_swapcoin = IncomingSwapCoin::new(
                maker_funded_multisig_privkey,
                maker_funded_other_multisig_pubkey,
                my_receivers_contract_tx.clone(),
                next_contract_redeemscript.clone(),
                hashlock_privkey,
                maker_funding_tx_value,
            )?;
            incoming_swapcoin.hash_preimage = Some(self.ongoing_swap_state.active_preimage);
            incoming_swapcoins.push(incoming_swapcoin);
        }

        Ok(incoming_swapcoins)
    }

}
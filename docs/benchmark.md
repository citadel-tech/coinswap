# Hotpath Benchmarking

This repo supports lightweight profiling via the `hotpath` crate.

When Hotpath is enabled, the taker and maker will:

- Write a JSON profiling report under `<DATA_DIR>/hotpath/`.
- Print summary tables for `functions_timing` and `functions_alloc` after the swap.

These instructions and the example tables below come from a swap performed on our custom Signet.

## Building With Hotpath

Hotpath requires compiling with features:

- `hotpath` for timing
- `hotpath-alloc` for allocation metrics

## Running The Taker With Hotpath

Generic command:

```bash
cargo run --bin taker --features 'hotpath hotpath-alloc' -- \
	-d <TAKER_DATA_DIR> \
	-a <RPC_USER>:<RPC_PASS> \
	-r <BITCOIND_RPC_ADDR:PORT> \
	-z <BITCOIND_ZMQ_ENDPOINT> \
	coinswap --protocol taproot --hotpath
```

Notes:

- Reports are written to `<TAKER_DATA_DIR>/hotpath/*.json`.
- The console prints a table when the swap finishes.

## Running The Maker(s) With Hotpath

Generic command:

```bash
cargo run --bin makerd --features 'hotpath hotpath-alloc' -- \
	-d <MAKER_DATA_DIR> \
	-a <RPC_USER>:<RPC_PASS> \
	-r <BITCOIND_RPC_ADDR:PORT> \
	-z <BITCOIND_ZMQ_ENDPOINT> \
	--hotpath
```

Notes:

- Reports are written to `<MAKER_DATA_DIR>/hotpath/*.json`.
- If you run multiple makers (e.g. `m1`, `m2`), you get one report per maker data directory.

## Current Benchmark Tables on custom signet -:
The tables below show:

- `functions_timing`: total time
- `functions_alloc`: total allocated

### Taker's Benchmarks (swap with 2 maker):
```
[hotpath] functions_timing (elapsed 1254.66 s)
name                                                                               calls        total          avg          p95    %total
-----------------------------------------------------------------------------------------------------------------------------------------
coinswap_taker_swap                                                                    1    1254.66 s    1254.67 s    1255.20 s   100.00%
coinswap::taker::api::start_coinswap                                                   1    1254.66 s    1254.67 s    1255.20 s   100.00%
coinswap::watch_tower::zmq_backend::poll                                         114548537    1218.59 s     10.64 µs     16.05 µs    97.12%
coinswap::watch_tower::zmq_backend::recv_event                                   114548537    1168.99 s     10.21 µs     15.46 µs    93.17%
coinswap::watch_tower::nostr_discovery::connect_and_run_once                           4    1090.19 s     272.56 s     274.34 s    86.89%
coinswap::watch_tower::nostr_discovery::read_event_loop                                4    1083.93 s     270.95 s     273.00 s    86.39%
coinswap::wallet::api::sweep_incoming_swapcoins                                        1     641.02 s     640.76 s     641.02 s    51.09%
coinswap::taker::taproot_swap::exchange_taproot                                        1     606.62 s     606.40 s     606.66 s    48.35%
coinswap::taker::taproot_swap::funding_broadcast                                       1     590.16 s     590.29 s     590.56 s    47.04%
coinswap::taker::api::net_wait_for_confirmation                                        1     590.15 s     590.29 s     590.56 s    47.04%
coinswap::wallet::api::wait_for_tx_confirmation                                        1     550.39 s     550.56 s     550.83 s    43.87%
coinswap::taker::api::net_connect                                                      4      13.64 s       3.41 s       6.33 s     1.09%
coinswap::taker::api::finalize_with_retry                                              1       6.30 s       6.30 s       6.30 s     0.50%
coinswap::taker::api::finalize_swap                                                    1       6.30 s       6.30 s       6.30 s     0.50%
coinswap::taker::api::finalize_exchange_privkeys                                       1       6.30 s       6.30 s       6.30 s     0.50%
coinswap::taker::api::net_handshake                                                    4       4.56 s       1.14 s       1.43 s     0.36%
coinswap::taker::api::funding_initialize                                               1     30.51 ms     30.52 ms     30.52 ms     0.00%
coinswap::taker::taproot_swap::funding_create_taproot                                  1     28.78 ms     28.78 ms     28.79 ms     0.00%
coinswap::wallet::funding::create_funding_txes                                         1     26.24 ms     26.24 ms     26.25 ms     0.00%
coinswap::wallet::api::send_tx                                                         2     18.37 ms      9.18 ms     11.06 ms     0.00%
coinswap::wallet::swapcoin::sign_spend_transaction                                     1      8.66 ms      8.66 ms      8.67 ms     0.00%
coinswap::wallet::swapcoin::sign_taproot_spend                                         1      8.65 ms      8.65 ms      8.65 ms     0.00%
coinswap::wallet::spend::spend_coins                                                   1      6.83 ms      6.83 ms      6.84 ms     0.00%
coinswap::protocol::musig_interface::generate_partial_signature_compat                 2      3.88 ms      1.94 ms      2.01 ms     0.00%
coinswap::protocol::musig2::generate_partial_signature                                 2      2.90 ms      1.45 ms      1.52 ms     0.00%
coinswap::taker::api::persist_swap                                                     8      2.01 ms    250.84 µs    540.67 µs     0.00%
coinswap::taker::taproot_verification::verify_maker_taproot_contract                   2      1.99 ms    994.05 µs      1.00 ms     0.00%
coinswap::protocol::musig_interface::aggregate_partial_signatures_compat               1      1.26 ms      1.26 ms      1.26 ms     0.00%
coinswap::protocol::musig_interface::generate_new_nonce_pair_compat                    2      1.20 ms    602.37 µs    610.30 µs     0.00%
coinswap::protocol::musig2::aggregate_partial_signatures                               1      1.15 ms      1.15 ms      1.15 ms     0.00%
coinswap::protocol::musig2::generate_new_nonce_pair                                    2      1.09 ms    546.05 µs    553.98 µs     0.00%
coinswap::protocol::musig_interface::get_aggregated_pubkey_compat                      1      1.03 ms      1.03 ms      1.03 ms     0.00%
coinswap::watch_tower::watcher::handle_command                                         3      1.00 ms    334.02 µs    444.93 µs     0.00%
coinswap::protocol::musig2::get_aggregated_pubkey                                      1    801.50 µs    801.54 µs    801.79 µs     0.00%
coinswap::taker::api::populate_success_outcomes                                        1    671.71 µs    671.49 µs    671.74 µs     0.00%
coinswap::watch_tower::nostr_discovery::handle_relay_message                           6    625.79 µs    104.31 µs    206.21 µs     0.00%
coinswap::taker::taproot_swap::exchange_create_incoming                                1    454.42 µs    454.53 µs    454.65 µs     0.00%
coinswap::taker::api::finalize_persist_incoming                                        2    419.25 µs    209.60 µs    215.04 µs     0.00%
coinswap::wallet::api::coin_select                                                     1    341.67 µs    341.63 µs    341.76 µs     0.00%
coinswap::taker::api::persist_build_record                                             8    222.83 µs     27.85 µs     44.77 µs     0.00%
coinswap::watch_tower::utils::parse_fidelity_event                                     4     41.58 µs     10.40 µs     12.75 µs     0.00%
coinswap::wallet::api::list_all_utxo_spend_info                                       21     34.83 µs      1.66 µs      5.21 µs     0.00%
coinswap::taker::taproot_swap::exchange_build_from_outgoing                            1     23.42 µs     23.42 µs     23.42 µs     0.00%
coinswap::wallet::api::get_balances                                                    2     16.00 µs      8.00 µs     10.50 µs     0.00%
coinswap::protocol::musig_interface::get_aggregated_nonce_compat                       1     15.88 µs     15.88 µs     15.88 µs     0.00%
coinswap::taker::api::min_expected_amount_for_hop                                      2      2.04 µs      1.02 µs      1.08 µs     0.00%
coinswap::protocol::taproot_messages::tap_tweak_scalar                                 1       958 ns       958 ns       958 ns     0.00%
coinswap::protocol::taproot_messages::new                                              2       249 ns       124 ns       166 ns     0.00%
coinswap::protocol::taproot_messages::from_bytes                                       1         0 ns         0 ns         0 ns     0.00%
```

```
[hotpath] functions_alloc (elapsed 1254.66 s)
name                                                                               calls          total            avg    %total
---------------------------------------------------------------------------------------------------------------------------------
coinswap::taker::api::net_wait_for_confirmation                                        1         1.3 MB         1.3 MB    47.97%
coinswap::watch_tower::nostr_discovery::connect_and_run_once                           4       568.2 KB       142.1 KB    19.83%
coinswap::wallet::api::sweep_incoming_swapcoins                                        1       204.7 KB       204.7 KB     7.14%
coinswap::taker::api::start_coinswap                                                   1       170.8 KB       170.8 KB     5.96%
coinswap::wallet::api::wait_for_tx_confirmation                                        1       112.0 KB       112.0 KB     3.91%
coinswap::wallet::funding::create_funding_txes                                         1        79.6 KB        79.6 KB     2.78%
coinswap::taker::api::net_handshake                                                    4        64.4 KB        16.1 KB     2.25%
coinswap::taker::taproot_swap::exchange_taproot                                        1        59.7 KB        59.7 KB     2.08%
coinswap::taker::api::persist_swap                                                     8        35.6 KB         4.4 KB     1.24%
coinswap::taker::api::finalize_exchange_privkeys                                       1        34.6 KB        34.6 KB     1.21%
coinswap::wallet::api::list_all_utxo_spend_info                                       21        26.7 KB         1.3 KB     0.93%
coinswap::taker::api::finalize_persist_incoming                                        2        20.9 KB        10.5 KB     0.73%
coinswap::watch_tower::nostr_discovery::read_event_loop                                4        14.9 KB         3.7 KB     0.52%
coinswap::taker::api::funding_initialize                                               1        14.3 KB        14.3 KB     0.50%
coinswap::wallet::api::send_tx                                                         2        10.5 KB         5.2 KB     0.37%
coinswap::wallet::api::coin_select                                                     1        10.3 KB        10.3 KB     0.36%
coinswap::wallet::spend::spend_coins                                                   1         9.6 KB         9.6 KB     0.34%
coinswap::taker::taproot_swap::funding_broadcast                                       1         8.7 KB         8.7 KB     0.30%
coinswap::watch_tower::watcher::handle_command                                         3         8.3 KB         2.8 KB     0.29%
coinswap::taker::api::populate_success_outcomes                                        1         5.9 KB         5.9 KB     0.21%
coinswap::taker::taproot_verification::verify_maker_taproot_contract                   2         5.7 KB         2.9 KB     0.20%
coinswap::taker::taproot_swap::funding_create_taproot                                  1         5.6 KB         5.6 KB     0.20%
coinswap::wallet::api::get_balances                                                    2         3.7 KB         1.8 KB     0.13%
coinswap::taker::taproot_swap::exchange_create_incoming                                1         3.4 KB         3.4 KB     0.12%
coinswap::watch_tower::nostr_discovery::handle_relay_message                           6         3.4 KB          578 B     0.12%
coinswap::wallet::swapcoin::sign_taproot_spend                                         1         3.2 KB         3.2 KB     0.11%
coinswap::taker::api::persist_build_record                                             8         2.8 KB          360 B     0.10%
coinswap::taker::api::net_connect                                                      4         1.2 KB          300 B     0.04%
coinswap::taker::taproot_swap::exchange_build_from_outgoing                            1         1.2 KB         1.2 KB     0.04%
coinswap::protocol::musig2::get_aggregated_pubkey                                      1          552 B          552 B     0.02%
coinswap::taker::api::finalize_swap                                                    1          256 B          256 B     0.01%
coinswap::wallet::swapcoin::sign_spend_transaction                                     1          170 B          170 B     0.01%
coinswap_taker_swap                                                                    1           54 B           54 B     0.00%
coinswap::protocol::taproot_messages::tap_tweak_scalar                                 1           32 B           32 B     0.00%
coinswap::protocol::musig2::aggregate_partial_signatures                               1            0 B            0 B     0.00%
coinswap::protocol::musig2::generate_new_nonce_pair                                    2            0 B            0 B     0.00%
coinswap::protocol::musig2::generate_partial_signature                                 2            0 B            0 B     0.00%
coinswap::protocol::musig_interface::aggregate_partial_signatures_compat               1            0 B            0 B     0.00%
coinswap::protocol::musig_interface::generate_new_nonce_pair_compat                    2            0 B            0 B     0.00%
coinswap::protocol::musig_interface::generate_partial_signature_compat                 2            0 B            0 B     0.00%
coinswap::protocol::musig_interface::get_aggregated_nonce_compat                       1            0 B            0 B     0.00%
coinswap::protocol::musig_interface::get_aggregated_pubkey_compat                      1            0 B            0 B     0.00%
coinswap::protocol::taproot_messages::from_bytes                                       1            0 B            0 B     0.00%
coinswap::protocol::taproot_messages::new                                              2            0 B            0 B     0.00%
coinswap::taker::api::finalize_with_retry                                              1            0 B            0 B     0.00%
coinswap::taker::api::min_expected_amount_for_hop                                      2            0 B            0 B     0.00%
coinswap::watch_tower::utils::parse_fidelity_event                                     4            0 B            0 B     0.00%
coinswap::watch_tower::zmq_backend::poll                                         114548537            0 B            0 B     0.00%
coinswap::watch_tower::zmq_backend::recv_event                                   114548537            0 B            0 B     0.00%
```

### Maker's Benchmarks:
```
[hotpath] functions_timing (elapsed 1505.13 s)
name                                                                               calls        total          avg          p95    %total
-----------------------------------------------------------------------------------------------------------------------------------------
coinswap_maker_swap                                                                    1    1505.13 s    1504.85 s    1505.39 s   100.00%
coinswap::maker::api::sweep_incoming_swapcoins                                         1     282.22 s     282.26 s     282.39 s    18.75%
coinswap::maker::api::setup_fidelity_bond                                              1     163.87 s     163.81 s     163.88 s    10.89%
coinswap::maker::server::read_message                                                 27      32.17 s       1.19 s       1.48 s     2.14%
coinswap::maker::server::handle_connection                                             7      30.00 s       4.29 s       4.97 s     1.99%
coinswap::maker::api::init                                                             1       6.55 s       6.55 s       6.55 s     0.44%
coinswap::maker::api::sync_and_save_wallet                                             1    823.11 ms    822.87 ms    823.13 ms     0.05%
coinswap::maker::api::check_swap_liquidity                                             1    796.58 ms    796.66 ms    796.92 ms     0.05%
coinswap::maker::handlers::handle_message                                             20     56.45 ms      2.82 ms      1.03 ms     0.00%
coinswap::maker::handlers::handle_taproot_dispatch                                     2     47.47 ms     23.74 ms     46.47 ms     0.00%
coinswap::maker::taproot_handlers::handle_taproot_message                              2     47.28 ms     23.63 ms     46.30 ms     0.00%
coinswap::maker::taproot_handlers::process_taproot_contract                            1     46.29 ms     46.28 ms     46.30 ms     0.00%
coinswap::maker::api::create_funding_transaction                                       1     32.85 ms     32.86 ms     32.87 ms     0.00%
coinswap::maker::api::broadcast_transaction                                            1      7.44 ms      7.44 ms      7.44 ms     0.00%
coinswap::maker::handlers::handle_get_offer                                            6      4.81 ms    801.71 µs    825.34 µs     0.00%
coinswap::maker::handlers::handle_swap_details                                         4      3.23 ms    807.94 µs    830.98 µs     0.00%
coinswap::maker::api::drain_idle_swaps                                               445      1.53 ms      3.44 µs      5.63 µs     0.00%
coinswap::maker::taproot_handlers::process_taproot_handover                            1    982.17 µs    982.27 µs    982.53 µs     0.00%
coinswap::maker::taproot_verification::verify_taproot_contract_data                    1    948.25 µs    948.48 µs    948.74 µs     0.00%
coinswap::maker::handlers::handle_taker_hello                                          8    919.17 µs    114.90 µs    154.75 µs     0.00%
coinswap::maker::api::save_incoming_swapcoin                                           2    513.29 µs    256.67 µs    288.00 µs     0.00%
coinswap::maker::api::verify_contract_tx_on_chain                                      1    511.25 µs    511.36 µs    511.49 µs     0.00%
coinswap::maker::taproot_verification::verify_taproot_privkey_handover                 1    482.42 µs    482.43 µs    482.56 µs     0.00%
coinswap::maker::server::send_message                                                 20    319.04 µs     15.95 µs     23.26 µs     0.00%
coinswap::maker::taproot_handlers::emit_maker_success_report                           1    249.12 µs    249.15 µs    249.22 µs     0.00%
coinswap::maker::api::save_outgoing_swapcoin                                           1    176.79 µs    176.83 µs    176.90 µs     0.00%
coinswap::maker::api::validate_swap_parameters                                         4     76.83 µs     19.21 µs     20.13 µs     0.00%
coinswap::maker::server::bind_port_retry                                               2     62.00 µs     31.01 µs     55.07 µs     0.00%
coinswap::maker::handlers::restore_state_if_needed                                     2     58.08 µs     29.05 µs     38.11 µs     0.00%
coinswap::maker::server::spawn_nostr_broadcast_thread                                  1     38.33 µs     38.32 µs     38.34 µs     0.00%
coinswap::maker::api::unwatch_outpoint                                                 4      6.92 µs      1.73 µs      6.59 µs     0.00%
coinswap::maker::api::register_watch_outpoint                                          2      1.50 µs       750 ns      1.38 µs     0.00%
```
```
[hotpath] functions_alloc (elapsed 1505.13 s)
name                                                                               calls          total            avg    %total
---------------------------------------------------------------------------------------------------------------------------------
coinswap::maker::api::setup_fidelity_bond                                              1         1.0 MB         1.0 MB    40.30%
coinswap::maker::api::sweep_incoming_swapcoins                                         1       569.8 KB       569.8 KB    22.13%
coinswap::maker::api::check_swap_liquidity                                             1       230.7 KB       230.7 KB     8.96%
coinswap::maker::api::sync_and_save_wallet                                             1       225.0 KB       225.0 KB     8.74%
coinswap::maker::api::init                                                             1       197.7 KB       197.7 KB     7.68%
coinswap::maker::api::create_funding_transaction                                       1       185.0 KB       185.0 KB     7.18%
coinswap::maker::api::validate_swap_parameters                                         4        29.6 KB         7.4 KB     1.15%
coinswap::maker::api::save_incoming_swapcoin                                           2        21.0 KB        10.5 KB     0.81%
coinswap::maker::taproot_handlers::process_taproot_contract                            1        15.2 KB        15.2 KB     0.59%
coinswap::maker::api::save_outgoing_swapcoin                                           1        11.7 KB        11.7 KB     0.46%
coinswap::maker::server::send_message                                                 20        10.9 KB          558 B     0.42%
coinswap::maker::handlers::handle_get_offer                                            6         6.8 KB         1.1 KB     0.26%
coinswap::maker::handlers::handle_taker_hello                                          8         5.7 KB          735 B     0.22%
coinswap::maker::api::broadcast_transaction                                            1         5.6 KB         5.6 KB     0.22%
coinswap::maker::handlers::handle_swap_details                                         4         3.8 KB          983 B     0.15%
coinswap::maker::api::verify_contract_tx_on_chain                                      1         3.7 KB         3.7 KB     0.14%
coinswap::maker::taproot_verification::verify_taproot_contract_data                    1         2.9 KB         2.9 KB     0.11%
coinswap::maker::server::read_message                                                 27         2.8 KB          107 B     0.11%
coinswap::maker::handlers::restore_state_if_needed                                     2         2.5 KB         1.3 KB     0.10%
coinswap::maker::taproot_handlers::emit_maker_success_report                           1         2.5 KB         2.5 KB     0.10%
coinswap::maker::api::register_watch_outpoint                                          2         1.5 KB          748 B     0.06%
coinswap::maker::server::handle_connection                                             7          830 B          118 B     0.03%
coinswap::maker::server::spawn_nostr_broadcast_thread                                  1          770 B          770 B     0.03%
coinswap::maker::taproot_verification::verify_taproot_privkey_handover                 1          688 B          688 B     0.03%
coinswap::maker::taproot_handlers::process_taproot_handover                            1          312 B          312 B     0.01%
coinswap::maker::handlers::handle_taproot_dispatch                                     2          288 B          144 B     0.01%
coinswap::maker::api::drain_idle_swaps                                               445           64 B            0 B     0.00%
coinswap::maker::api::unwatch_outpoint                                                 4            0 B            0 B     0.00%
coinswap::maker::handlers::handle_message                                             20            0 B            0 B     0.00%
coinswap::maker::server::bind_port_retry                                               2            0 B            0 B     0.00%
coinswap::maker::taproot_handlers::handle_taproot_message                              2            0 B            0 B     0.00%
coinswap_maker_swap                                                                    1            N/A            N/A       N/A
```
#![cfg(feature = "integration-test")]

#[macro_use]
mod test_framework;

mod abort1;
mod abort2_case1;
mod abort2_case2;
mod abort2_case3;
mod abort3_case1;
mod abort3_case2;
mod abort3_case3;
mod concurrent_takers;
mod electrum_swap;
mod fidelity;
mod fidelity_renewal;
mod fidelity_timelock_violation;
mod funding_dynamic_splits;
#[cfg(feature = "hotpath")]
mod hotpath_profile;
mod liquidity_test;
mod malice1;
mod malice2;
mod multi_taker;
mod offerbook_sync_race;
mod skip_funding_recovery;
mod standard_swap;
mod taker_cli;
mod taproot_concurrent_takers;
mod taproot_hashlock_recovery;
mod taproot_maker_abort1;
mod taproot_maker_abort2;
mod taproot_maker_abort3;
mod taproot_multi_maker;
mod taproot_multi_taker;
mod taproot_reboot_recovery;
mod taproot_swap;
mod taproot_taker_abort1;
mod taproot_taker_abort2;
mod taproot_taker_abort3;
mod taproot_timelock_recovery;
mod wallet_backup;

mod concurrent_takers;
mod electrum_swap;
mod fidelity_timelock_violation;
mod funding_dynamic_splits;
#[cfg(feature = "hotpath")]
mod hotpath_profile;
mod legacy_malformed_contract;
mod liquidity_test;
mod offerbook_sync_race;
mod taker_cli;
mod taproot_concurrent_takers;
mod taproot_contract_validation;
mod taproot_taker_contract_validation;
mod utxo_behavior;
mod wallet_backup;

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
mod fidelity;
mod fidelity_renewal;
mod malice1;
mod malice2;
mod multi_taker;
mod skip_funding_recovery;
mod standard_swap;
mod taproot_hashlock_recovery;
mod taproot_maker_abort1;
mod taproot_maker_abort2;
mod taproot_maker_abort3;
mod taproot_multi_maker;
mod taproot_multi_taker;
mod taproot_swap;
mod taproot_taker_abort1;
mod taproot_taker_abort2;
mod taproot_taker_abort3;
mod taproot_timelock_recovery;
mod wallet_backup;

mod concurrent_takers;
mod fidelity_timelock_violation;
mod funding_dynamic_splits;
#[cfg(feature = "hotpath")]
mod hotpath_swap;
mod liquidity_test;
mod offerbook_sync_race;
mod taker_cli;
mod taproot_concurrent_takers;
mod utxo_behavior;

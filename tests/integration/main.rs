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
mod fidelity_timelock_violation;
mod funding_dynamic_splits;
mod liquidity_test;
mod malice1;
mod malice2;
mod multi_taker;
mod standard_swap;
mod taker_cli;
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
mod unified_recovery;
mod unified_swap;
mod utxo_behavior;
mod wallet_backup;

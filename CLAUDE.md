# CLAUDE.md

This file gives quick working guidance for this repository.

## Project Snapshot

Coinswap is a Rust implementation of a trustless, self-custodial Bitcoin atomic swap protocol. The codebase currently supports both protocol families in parallel:

- `Legacy` swap flow based on ECDSA and script-enforced contracts
- `Taproot-Musig2` swap flow based on scriptless aggregate signatures

The main actors are:

- `Makers`, who run swap services and provide liquidity
- `Takers`, who coordinate swaps and route messages through makers

The project also includes wallet, security, and watch-tower components for recovery and monitoring.

## Repository Layout

- `src/protocol/`: protocol messages, contracts, and signature handling for both protocol versions
- `src/maker/`: maker server logic, handlers, RPC, and swap tracking
- `src/taker/`: taker client logic, offer selection, swap execution, and route management
- `src/wallet/`: wallet storage, funding, fidelity bonds, backup, and RPC integration
- `src/watch_tower/`: monitoring and recovery support through ZMQ, REST, and discovery backends
- `src/security.rs`: wallet encryption and serialization helpers
- `tests/integration/`: regtest integration scenarios for aborts, malice, recovery, fidelity, and Taproot flows
- `examples/`: small runnable examples for wallet and taker usage

## Current Docs

- `README.md`: project overview, setup, binaries, and current announcements
- `CONTRIBUTING.md`: contribution flow, code review expectations, security policy, and AI contribution policy
- `docs/bitcoind.md`: Bitcoin Core setup
- `docs/tor.md`: Tor setup
- `docs/docker.md`: Docker-based setup and deployment
- `docs/makerd.md`, `docs/maker-cli.md`, `docs/taker.md`: binary-specific docs
- `docs/taproot.md`, `docs/demo-v1.md`, `docs/demo-v2.md`, `docs/workshop.md`: protocol and demo material

## Build And Test

The repo uses the stable Rust toolchain from `rust-toolchain.toml`.

```bash
cargo build --release
cargo test
cargo test --features integration-test -- --nocapture
```

Formatting and linting expectations:

```bash
cargo +nightly fmt --all
cargo +stable clippy --all-features --lib --bins --tests -- -D warnings
cargo +stable clippy --examples -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo +nightly doc --all-features --document-private-items --no-deps
```

## Key Working Rules

- When changing protocol logic, update both legacy and Taproot paths unless the change is explicitly version-specific.
- Keep test-only behavior behind `#[cfg(feature = "integration-test")]`.
- Integration tests spin up regtest environments and simulate end-to-end swap behavior.
- Bitcoin Core requires wallet sync, `-txindex=1`, and ZMQ endpoints for the wallet and watch-tower flows.
- Tor is part of the normal network path and should be treated as a first-class dependency.

## Useful Test Entry Points

- `cargo test --test integration standard_swap --features integration-test -- --nocapture`
- `cargo test --test integration taproot_swap --features integration-test -- --nocapture`
- `cargo test --test integration abort1 --features integration-test -- --nocapture`
- `cargo test --test integration taproot_timelock_recovery --features integration-test -- --nocapture`

## Contribution Notes

- Follow the guidance in `CONTRIBUTING.md`
- Use the pre-commit hook in `git_hooks/pre-commit` if you want local linting on commit
- The project is security-sensitive and does not want low-effort AI-generated changes

## External References

- CoinSwap protocol specification: https://github.com/citadel-tech/Coinswap-Protocol-Specification
- Deep Wiki: https://deepwiki.com/citadel-tech/coinswap
- GUI taker app: https://github.com/citadel-tech/taker-app
- GUI maker manager: https://github.com/citadel-tech/maker-dasboard

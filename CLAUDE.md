# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Coinswap is a trustless, peer-to-peer atomic swap protocol implementation in Rust that enables decentralized Bitcoin swaps through a marketplace without central intermediaries. The system supports both **Legacy (ECDSA)** and **Taproot-Musig2** protocol versions running in parallel.

**Key actors:**
- **Makers**: Swap service providers running as `makerd` daemon, earning fees for liquidity
- **Takers**: Clients initiating swaps via `taker` CLI, coordinating multi-hop routes through makers

## Development Commands

### Building
```bash
cargo build --release              # Build all binaries (makerd, maker-cli, taker)
cargo build --release --bin taker  # Build specific binary
```

### Testing
```bash
# Run full integration test suite (requires `integration-test` feature)
cargo test --features integration-test

# Run specific test with output
cargo test --test standard_swap --features integration-test -- --nocapture

# Run with coverage
cargo llvm-cov nextest --lib --bins --tests --features integration-test --no-capture
cargo llvm-cov report --cobertura --output-path ./coverage/reports/cobertura.xml
```

**Important:** Integration tests spawn isolated regtest Bitcoin nodes and simulate complete swap scenarios. They require the `integration-test` feature flag which adjusts timeouts and constants for faster test execution.

### Linting
```bash
# Format (requires nightly)
cargo +nightly fmt --all

# Check formatting without modifying
cargo +nightly fmt --all -- --check

# Clippy (must pass with zero warnings)
cargo +stable clippy --all-features --lib --bins --tests -- -D warnings
cargo +stable clippy --examples -- -D warnings

# Documentation check
RUSTDOCFLAGS="-D warnings" cargo +nightly doc --all-features --document-private-items --no-deps
```

**CI Requirements:** All PRs must pass `cargo +nightly fmt --check` and clippy with `-D warnings`. Use the git pre-commit hook:
```bash
ln -s ../../git_hooks/pre-commit .git/hooks/pre-commit
```

## Architecture

### Core Modules

- **`src/protocol/`**: Contract transactions (HTLC/HLTC), message structures, cryptographic operations
  - `messages.rs` / `messages2.rs`: Message flow for legacy/Taproot protocols
  - `contract.rs` / `contract2.rs`: Script-evaluated HTLC (legacy) and scriptless Musig2 contracts (Taproot)
  - `musig2/`: Taproot-specific aggregate signature handling

- **`src/wallet/`**: Shared Bitcoin wallet for both takers and makers
  - `api.rs`: Public wallet API with UTXO management
  - `rpc.rs`: Bitcoin Core RPC interaction (requires `-txindex=1`)
  - `fidelity.rs`: Fidelity bond creation/validation for sybil resistance
  - `swapcoin.rs` / `swapcoin2.rs`: Swap-specific coin tracking (legacy/Taproot)

- **`src/maker/`**: Maker server implementation
  - `api.rs`: State machine for maker swap coordination
  - `handlers.rs` / `handlers2.rs`: Protocol message handlers (legacy/Taproot)
  - Test-only `MakerBehavior` enum (requires `#[cfg(feature = "integration-test")]`) simulates failure modes

- **`src/taker/`**: Taker client implementation
  - `api.rs`: Swap coordination and route management
  - `routines.rs`: Connection handling and maker communication
  - `offers.rs`: Maker discovery and selection

- **`src/watch_tower/`**: Contract monitoring and recovery
  - ZMQ subscription to Bitcoin Core (`zmqpubrawtx`, `zmqpubrawblock`)
  - Detects contract breaches and triggers recovery transactions

- **`src/security.rs`**: AES-256-GCM encryption for wallet data with CBOR/JSON serialization

### Message Protocol Flow

Messages follow a sender/receiver pattern where each party acts as both:
- **Sender**: Sending coins out of their wallet
- **Receiver**: Receiving coins into their wallet

Three-part contract negotiation per hop:
1. Request (taker → maker)
2. Response (maker → taker with contract details)
3. Proof-of-funding (both parties verify contract setup)

See [src/protocol/messages.rs](src/protocol/messages.rs) for detailed 3-hop example and message sequence.

### Legacy vs Taproot Differences

| Aspect | Legacy (v1) | Taproot (v2) |
|--------|------------|-------------|
| Key agreement | ECDSA signatures | Musig2 aggregate signatures |
| Contracts | Script-evaluated HTLC | Scriptless via musig2 |
| Modules | `messages.rs`, `contract.rs` | `messages2.rs`, `contract2.rs` |
| Address type | P2WPKH | P2TR |
| Test framework | `TestFramework::init()` | `TestFramework::init_taproot()` |

Both protocols run in parallel. When modifying protocol logic, ensure changes are made to both versions if applicable.

### Fidelity Bonds (Sybil Resistance)

Time-locked UTXOs demonstrating sybil-resistant reputation via coin locks:
- **Value formula**: `bond_value = locked_amount * (locktime_years)^1.3`
- **Script**: `pubkey OP_CHECKSIGVERIFY locktime OP_CLTV`
- **Derivation path**: `m/84'/0'/0'/2` (P2WPKH)
- Implementation: [src/wallet/fidelity.rs](src/wallet/fidelity.rs)

### Fee Calculation

Located in [src/maker/api.rs](src/maker/api.rs):
- `base_fee`: Fixed cost per swap
- `amount_relative_fee_pct`: Percentage of swap amount
- `TIME_RELATIVE_FEE_PCT`: Duration-based fee

**Formula**: `total_fee = base_fee + (amount * amt_pct)/100 + (amount * locktime * time_pct)/100`

## Testing Infrastructure

### Test Framework

The test framework ([tests/test_framework/mod.rs](tests/test_framework/mod.rs)) spawns isolated regtest environments:
- Downloads/caches Bitcoin binaries in `bin/bitcoin-{VERSION}/` if not present
- Creates regtest nodes with ZMQ subscriptions
- Spawns maker/taker instances with configurable behaviors
- Supports both legacy and Taproot protocols

**Key functions:**
- `TestFramework::init()`: Legacy protocol setup
- `TestFramework::init_taproot()`: Taproot-specific setup

### Maker Behavior Simulation

Test-only enum in [src/maker/api.rs](src/maker/api.rs) (requires `#[cfg(feature = "integration-test")]`):
- `Normal`: Standard operation
- `CloseAtReqContractSigsForSender`: Abort during message phase
- `BroadcastContractAfterSetup`: Trigger recovery paths

### Running Specific Tests

```bash
# Standard successful swap (good starting point)
cargo test --test standard_swap --features integration-test -- --nocapture

# Abort scenarios (maker/taker failures)
cargo test --test abort1 --features integration-test -- --nocapture
cargo test --test taproot_taker_abort1 --features integration-test -- --nocapture

# Malicious behavior and recovery
cargo test --test malice1 --features integration-test -- --nocapture
cargo test --test taproot_timelock_recovery --features integration-test -- --nocapture
```

## Development Patterns

### Concurrency and State Management

Heavy use of `Arc<Mutex<T>>` and `Arc<RwLock<T>>` for shared state:
```rust
// Accessing maker wallet
let wallet = maker.get_wallet().write().unwrap();  // or .read().unwrap()

// Always sync and save after state changes
wallet.sync_and_save()?;
```

Watch tower operates on separate thread via `WatchService`.

### Error Handling

Custom error types per module (not generic):
- `maker::error::MakerError`
- `taker::error::TakerError`
- `wallet::error::WalletError`
- `protocol::error::ProtocolError`

Recovery paths use `IDLE_CONNECTION_TIMEOUT` (60s tests, 15 min production).

### Feature Flags

The `integration-test` feature flag adjusts constants for faster testing:
- Timeouts: 60s (tests) vs 15 min (production) for `IDLE_CONNECTION_TIMEOUT`
- Fidelity update interval: 30s (tests) vs 600s/1 block (production)
- Fee percentages adjusted for faster test execution

When adding new timeouts/constants, use `#[cfg(feature = "integration-test")]` to provide test-specific values.

### Bitcoin Core Requirements

- Full node with `-txindex=1` for wallet sync
- ZMQ enabled: `-zmqpubrawtx=tcp://127.0.0.1:28332 -zmqpubrawblock=tcp://127.0.0.1:28333`
- RPC connection configured via `wallet::RPCConfig`
- All communication tunneled through Tor (check via `utill::check_tor_status()`)

## Adding Protocol Changes

1. **Modify message types**: [src/protocol/messages.rs](src/protocol/messages.rs) (legacy) or [messages2.rs](src/protocol/messages2.rs) (Taproot)
2. **Update handlers**: [src/maker/handlers.rs](src/maker/handlers.rs) or [handlers2.rs](src/maker/handlers2.rs)
3. **Add tests**: Create integration test in `tests/` using `TestFramework`
4. **Handle both versions**: Ensure changes work for both legacy and Taproot if applicable

## Project Binaries

- **`makerd`**: Maker daemon server ([src/bin/makerd.rs](src/bin/makerd.rs))
- **`maker-cli`**: RPC controller for makerd ([src/bin/maker-cli.rs](src/bin/maker-cli.rs))
- **`taker`**: Taker CLI client ([src/bin/taker.rs](src/bin/taker.rs))

## External Resources

- Protocol specifications: [Coinswap Protocol Specification](https://github.com/citadel-tech/Coinswap-Protocol-Specification)
- GUI client: [Taker App](https://github.com/citadel-tech/taker-app)
- Docker deployment: [docs/docker.md](docs/docker.md)
- Bitcoin setup: [docs/bitcoind.md](docs/bitcoind.md)
- Tor configuration: [docs/tor.md](docs/tor.md)

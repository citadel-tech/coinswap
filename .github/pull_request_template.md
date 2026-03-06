# Pull Request

## Description
<!-- Provide a clear and concise description of what this PR does and why it is needed. -->

## Related Issue(s)
Closes #<!-- replace with issue number if applicable -->
<!-- Add multiple lines if this PR closes multiple issues -->

## Type of Change
- [ ] Bug fix (non-breaking change which fixes an issue)
- [ ] New feature (non-breaking change which adds functionality)
- [ ] Breaking change (fix or feature that would cause existing functionality to not work as expected)
- [ ] Code refactor / performance improvement
- [ ] Documentation update only
- [ ] CI / Docker / Build changes
- [ ] Other (please describe):

<!-- If breaking change, describe the migration path or config changes required: -->

## Protocol Version(s) Affected
- [ ] Legacy (ECDSA — `messages.rs`, `contract.rs`, `handlers.rs`)
- [ ] Taproot-Musig2 (`messages2.rs`, `contract2.rs`, `handlers2.rs`)
- [ ] Both
- [ ] Neither (infrastructure / tooling only)

> **Note:** When modifying protocol logic, changes must be made to **both** versions unless the
> scope is explicitly version-specific.

## Affected Component(s)
- [ ] **makerd** (background daemon)
- [ ] **taker** (CLI client)
- [ ] **maker-cli** (command-line tool)
- [ ] Core protocol / cryptography / library
- [ ] **watch_tower** (contract monitoring & recovery)
- [ ] Docker / deployment scripts
- [ ] Tests / test framework
- [ ] Documentation (`docs/`)
- [ ] Other (please specify):

## Checklist

### Code Quality
- [ ] I ran `cargo +nightly fmt --all` and committed the result
- [ ] I ran `cargo +stable clippy --all-features --lib --bins --tests -- -D warnings` with zero warnings
- [ ] I ran `cargo +stable clippy --examples -- -D warnings` with zero warnings
- [ ] I ran `RUSTDOCFLAGS="-D warnings" cargo +nightly doc --all-features --document-private-items --no-deps` with zero warnings
- [ ] Pre-commit git hook passes (`ln -s ../../git_hooks/pre-commit .git/hooks/pre-commit` if not already set)

### Testing
- [ ] All unit tests pass (`cargo test`)
- [ ] Integration tests pass (`cargo test --features integration-test`)
- [ ] Changes were manually tested on **regtest**
- [ ] End-to-end maker ↔ taker swap tested (where applicable)
- [ ] Test-only code is gated behind `#[cfg(feature = "integration-test")]`

### Documentation
- [ ] Relevant files in the `docs/` folder were updated
- [ ] Complex logic is commented

### Security & Privacy (Critical)
- [ ] This change preserves **trustlessness** and **atomic swap guarantees**
- [ ] No regression in **sybil resistance** or **fidelity bond** logic
- [ ] Tor anonymity and P2P message flow were reviewed
- [ ] No new attack vectors or trust assumptions introduced
- [ ] Edge cases and error handling considered
- [ ] ZMQ subscription integrity (raw tx/block feed) is unaffected
- [ ] `integration-test` feature flag is not reachable in production code paths

## How to Test
<!-- Step-by-step instructions so reviewers can verify quickly. -->

```bash
# Standard swap (good baseline)
cargo test --test standard_swap --features integration-test -- --nocapture

# Abort scenarios
cargo test --test abort1 --features integration-test -- --nocapture
cargo test --test taproot_taker_abort1 --features integration-test -- --nocapture

# Malicious behavior & recovery
cargo test --test malice1 --features integration-test -- --nocapture
cargo test --test taproot_timelock_recovery --features integration-test -- --nocapture

# Or run the full suite
cargo test --features integration-test
```

<!-- Add any additional manual steps specific to this PR
     (e.g. regtest node config, docker-setup, specific taker commands): -->

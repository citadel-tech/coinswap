---
name: Bug Report
about: Report a bug or unexpected behavior in CoinSwap
title: "bug: "
labels: ["bug"]
assignees: ""
---

## Description
<!-- A clear and concise description of the bug. -->

## Steps to Reproduce
1.
2.
3.

## Expected Behavior
<!-- What you expected to happen. -->

## Actual Behavior
<!-- What actually happened. Include the full error message, panic output, or unexpected result. -->

```
<!-- paste error / panic output here -->
```

## Environment

| Field | Value |
|---|---|
| CoinSwap version | <!-- `git rev-parse --short HEAD` or release tag --> |
| OS | <!-- e.g. Ubuntu 24.04, macOS 14 --> |
| Rust toolchain | <!-- `rustc --version && cargo --version` --> |
| Bitcoin Core version | <!-- `bitcoind --version` --> |
| Tor version | <!-- `tor --version` --> |
| Setup | <!-- Docker / Native --> |
| Network | <!-- regtest / mutinynet / mainnet --> |
| Role | <!-- Maker / Taker / Both --> |
| Protocol version | <!-- Legacy (ECDSA) / Taproot-Musig2 / Both --> |

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

## Logs
<!-- Paste relevant logs below. For integration tests, re-run with `-- --nocapture` to capture output. -->

<details>
<summary>Log output</summary>

```
<!-- paste logs here -->
```

</details>

## Additional Context
<!-- Any other context, screenshots, or links that might help diagnose the issue. -->

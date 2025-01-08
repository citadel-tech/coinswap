<div align="center">

<h1>Coinswap</h1>

<p>
    Functioning, minimal-viable binaries and libraries to perform a trustless, p2p <a href="https://gist.github.com/chris-belcher/9144bd57a91c194e332fb5ca371d0964">Maxwell-Belcher Coinswap Protocol</a>.
  </p>

<p>
    <!--
    <a href="https://crates.io/crates/coinswap"><img alt="Crate Info" src="https://img.shields.io/crates/v/coinswap.svg"/></a>
    <a href="https://docs.rs/coinswap"><img alt="API Docs" src="https://img.shields.io/badge/docs.rs-coinswap-green"/></a>
    -->
    <a href="https://github.com/citadel-tech/coinswap/blob/master/LICENSE"><img alt="MIT or Apache-2.0 Licensed" src="https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg"/></a>
    <a href="https://github.com/citadel-tech/coinswap/actions/workflows/build.yaml"><img alt="CI Status" src="https://github.com/citadel-tech/coinswap/actions/workflows/build.yaml/badge.svg"></a>
    <a href="https://github.com/citadel-tech/coinswap/actions/workflows/lint.yaml"><img alt="CI Status" src="https://github.com/citadel-tech/coinswap/actions/workflows/lint.yaml/badge.svg"></a>
    <a href="https://github.com/citadel-tech/coinswap/actions/workflows/test.yaml"><img alt="CI Status" src="https://github.com/citadel-tech/coinswap/actions/workflows/test.yaml/badge.svg"></a>
    <a href="https://codecov.io/github/citadel-tech/coinswap?branch=master">
    <img alt="Coverage" src="https://codecov.io/github/citadel-tech/coinswap/coverage.svg?branch=master">
    </a>
    <a href="https://blog.rust-lang.org/2023/12/28/Rust-1.75.0.html"><img alt="Rustc Version 1.75.0+" src="https://img.shields.io/badge/rustc-1.75.0%2B-lightgrey.svg"/></a>
  </p>
</div>

### ⚠️ Info

Coinswap v0.1.0 marketplace is now live on Testnet4.


### ⚠️ Warning

This library is currently under beta development and is in an experimental stage. There are known and unknown bugs. **Mainnet use is strictly NOT recommended.** 

# About

Coinswap is a decentralized [atomic swap](https://bitcoinops.org/en/topics/coinswap/) protocol that enables trustless swaps of Bitcoin UTXOs through a decentralized, Sybil-resistant marketplace.

While atomic swaps are not new, existing solutions are centralized, rely on large swap servers, and inherently have the service provider as a [single point of failure (SPOF)](https://en.wikipedia.org/wiki/Single_point_of_failure) for censorship and privacy attacks. This project aims to implement atomic swaps via a decentralized market-based protocol.

The project builds on the work of many predecessors and is a continuation of Bitcoin researcher Chris Belcher's [teleport-transactions](https://github.com/bitcoin-teleport/teleport-transactions). Since Belcher's prototype, the project has significantly matured, incorporating complete protocol handling, functional testing, Sybil resistance, and command-line applications, making it a practical swap solution for live networks.

Anyone can become a swap service provider (aka `Maker`) and earn fees by running the `makerd` app. Clients (aka `Takers`) can perform swaps with multiple makers using the `taker` app. A taker selects multiple makers from the market to swap with, splitting and routing swaps through various makers while maintaining privacy. 

The system is designed with a *smart-client-dumb-server* philosophy, minimizing server requirements. This allows any home node operator to run `makerd` on their node box. The protocol employs [fidelity bonds](https://github.com/JoinMarket-Org/joinmarket-clientserver/blob/master/docs/fidelity-bonds.md) as a Sybil and DoS resistance mechanism for the market. Takers will avoid swapping with makers holding expired or invalid fidelity bonds.

Takers, acting as smart clients, handle critical roles such as coordinating swap rounds, validating data, managing interactions with multiple makers, and recovering swaps in case of errors. Makers, acting as dumb servers, respond to taker queries and do not communicate with each other. Instead, the taker routes all inter-maker messages. All communication strictly occurs over Tor.

For more details on the protocol and market mechanisms, refer to the [Coinswap Protocol Specification](https://github.com/citadel-tech/Coinswap-Protocol-Specification).


# Run the apps
### ❗ Important

The project currently only compiles on Linux. Mac and Windows are not supported yet. To compile on Mac or Windows, consider using virtual machines.

### Dependencies

Ensure you have the following dependency installed before compiling.

```shell
sudo apt install build-essential automake libtool
```

The project also requires working `rust` and `cargo` installation to compile. Precompile binaries will be available soon. Cargo can be installed from [here](https://www.rust-lang.org/learn/get-started).

### Build and Install
```console
git clone https://github.com/citadel-tech/coinswap.git
cd coinswap
cargo build
```

After compilation you will get the binaries in the `./target/debug` folder. 

Install the required binaries:
```console
sudo cp ./target/debug/maker* /usr/local/bin/
sudo cp ./target/debug/taker /usr/local/bin/    
```

### Apps overview
This will install three binaries, `makerd`, `maker-cli` and `taker` in your system, which can be used to run both the server and the client.
Use the help command to for more information.

```console
makerd --help
maker-cli --help
taker --help
```

  `makerd`: The backend server daemon. This requires continuous uptime and connection to live bitcoin core RPC. App demo [here](https://github.com/citadel-tech/coinswap/blob/master/docs/app%20demos/makerd.md)
  
  `maker-cli`: The RPC controler of the server deamon. This can be used to manage the server, access internal wallet, see swap statistics, etc. App demo [here](https://github.com/citadel-tech/coinswap/blob/master/docs/app%20demos/maker-cli.md)
  
  `taker`: The swap client app. This acts as a regular bitcoin wallet with swap capability. App demo [here](https://github.com/citadel-tech/coinswap/blob/master/docs/app%20demos/taker.md)

All the apps will require a Bitcoin Core RPC connection running on testnet4. For running bitcoin instrcutions, see [here](https://github.com/citadel-tech/coinswap/blob/master/docs/app%20demos/bitcoind.md)

# [Dev Mode] Checkout the tests

Extensive functional testing to simulate various edge cases of the protocol, is covered. The [functional tests](./tests/) spawns 
a toy marketplace in Bitcoin regetst and plays out various protocol situation. Functional test logs are a good way to look at simulations of various
edge cases in the protocol, and how the taker and makers recover from failed swaps. 

The bitcoin binary used for testing is included [here](./bin/bitcoind).

### 💡 Tip

Replace the `bitcoind` binary to run the tests with your custom bitcoind build.

Each test in the [tests](./tests/) folder covers a different edge-case situation and demonstrates how the taker and makers recover
from various types of swap failures.

Run all the functional tests and see the logs:

```console
$ cargo test --features=integration-test -- --nocapture
```

A rust based [`TestFramework`](./tests/test_framework/mod.rs) (Inspired from the Bitcoin Core [testframeowrk](https://github.com/bitcoin/bitcoin/tree/master/test/functional)) has been designed to easily spawn the test situations, with many makers and takers. For example checkout the simple [`standard_swap` module](./tests/standard_swap.rs) to see how to simulate a simple swap case programatically. 

The functional tests is a good place for potential contributors to start tinkering and gathering context.

# Contributing

The project is under active development by developers at Citadel Tech. Any contribution for features, tests, docs and other fixes/upgrades is encouraged and welcomed. The maintainers will use the PR thread to provide quick reviews and suggestions and are generally proactive at merging good contributions.

Few directions for new contributors:

- The list of [issues](https://github.com/citadel-tech/coinswap/issues) is a good place to look for contributable tasks and open problems.

- Issues marked with [`good first issue`](https://github.com/citadel-tech/coinswap/issues?q=is%3Aopen+is%3Aissue+label%3A%22good+first+issue%22) are good places to get started for newbie Rust/Bitcoin devs.

- The [docs](./docs) are a good place to start reading up on the protocol.

- Reviewing [open PRs](https://github.com/citadel-tech/coinswap/pulls) are a good place to start gathering a contextual understanding of the codebase.

- Search for `TODO`s in the codebase to find in-line marked code todos and smaller improvements.

### Setting Up Git Hooks

The repo contains pre-commit githooks to do auto-linting before commits. Set up the pre-commit hook by running:

```bash
ln -s ../../git_hooks/pre-commit .git/hooks/pre-commit
```

## Community

The dev community lurks [here](https://discord.gg/Wz42hVmrrK).

Dev discussions predominantly happen via FOSS best practices, and by using Github as the major community forum.

The Issues, PRs and Discussions are where all the hard lifting happening.

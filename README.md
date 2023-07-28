# Teleport Transactions

<!-- TOC -->
- [State of the project](#state-of-the-project)
- [Installation/Build From Source](#installationbuild-from-source)
- [Docs](#docs)
- [Roadmap for the project](#roadmap-for-the-project)
- [Community](#community)
<!-- /TOC -->

Teleport Transactions is software aiming to improve the [privacy](https://en.bitcoin.it/wiki/Privacy) of [Bitcoin](https://en.bitcoin.it/wiki/Main_Page).

Suppose Alice has bitcoin and wants to send them with maximal privacy, so she creates a special kind of transaction.
For anyone looking at the blockchain her transaction appears completely normal with her coins seemingly going from Bitcoin address A to address B.
But in reality her coins end up in address Z which is entirely unconnected to either A or B.

Now imagine another user, Carol, who isn't too bothered by privacy and sends her bitcoin using a regular wallet.
But because Carol's transaction looks exactly the same as Alice's, anybody analyzing the blockchain must now deal with the possibility that Carol's transaction actually sent her coins to a totally unconnected address.
So Carol's privacy is improved even though she didn't change her behaviour, and perhaps had never even heard of this software.

In a world where advertisers, social media and other institutions want to collect all of Alice's and Carol's data, such privacy improvement is incredibly valuable.
And the doubt added to every transaction would greatly boost the [fungibility of Bitcoin](https://en.bitcoin.it/wiki/Fungibility) and so make it a better form of money.

Project design document: [Design for a CoinSwap Implementation for Massively Improving Bitcoin Privacy and Fungibility](https://gist.github.com/chris-belcher/9144bd57a91c194e332fb5ca371d0964)

## State of the project

The project is nearly usable, though it doesn't have all the necessary features yet.
It's a cli app written in rust as demo prototype of the Coinswap protocol laid out by [Chris Belcher](https://github.com/chris-belcher) with underlying subroutines and primitives.
The code written so far is published for developers and power users to play around with.
It doesn't have config files yet so you have to edit the source files to configure stuff.
It is possible to run it on mainnet, but only the brave will attempt that, and only with small amounts.

## Installation/Build From Source

1. Make sure you have [rust](https://www.rust-lang.org/) on your machine.
2. Clone the repo.
3. From within the directory do `cargo build`.
4. Test the binary with `cargo test`.
You need Bitcoin core to be running in `regtest` for `test_standard_coinswap` to pass.

Check [app_instructions.md](docs/app_instructions.md) for steps on how to create a vanilla coinswap with this implementation.

## Docs

Check [Developer Resources](docs/developer_resources.md) on information on the protocol and further reading.

## Roadmap for the project

* &#9745; learn rust
* &#9745; learn rust-bitcoin
* &#9745; design a protocol where all the features (vanilla coinswap, multi-tx coinswap, routed coinswap, branching routed coinswap, privkey handover) can be done, and publish to mailing list
* &#9745; code the simplest possible wallet, seed phrases "generate" and "recover", no fidelity bonds, everything is sybil attackable or DOS attackable for now, no RBF
* &#9745; implement creation and signing of traditional multisig
* &#9745; code makers and takers to support simple coinswap
* &#9745; code makers and takers to support multi-transaction coinswaps without any security (e.g. no broadcasting of contract transactions)
* &#9745; code makers and takers to support multi-hop coinswaps without security
* &#9745; write more developer documentation
* &#9744; set up a solution to mirror this repository somewhere else in case github rm's it like they did youtube-dl
* &#9745; implement and deploy fidelity bonds in joinmarket, to experiment and gain experience with the concept
* &#9745; add proper error handling to this project, as right now most of the time it will exit on anything unexpected
* &#9745; code security, recover from aborts and deveations
* &#9745; implement coinswap fees and taker paying for miner fees
* &#9745; add support for connecting to makers that arent on localhost, and tor support
* &#9745; code federated message board seeder servers
* &#9745; ALPHA RELEASE FOR TESTNET, REGTEST, SIGNET AND MAINNET (FOR THE BRAVE ONES)
* &#9745; have watchtower store data in a file, not in RAM
* &#9744; study ecdsa-2p and implement ecdsa-2p multisig so the coinswaps can look identical to regular txes
* &#9744; have taker store the progress of a coinswap to file, so that the whole process can be easily paused and started
* &#9744; add automated incremental backups for wallet files, because seed phrases aren't enough to back up these wallets
* &#9744; code fidelity bonds
* &#9744; add support precomputed RBF fee-bumps, so that txes can always be confirmed regardless of the block space market
* &#9744; automated tests (might be earlier in case its useful in test driven development)
* &#9744; move wallet files and config to its own data directory ~/.teleport/
* &#9744; add collateral inputs to receiver contract txes
* &#9744; implement encrypted contract txes for watchtowers, so that watchtowers can do their job without needing to know the addresses involved
* &#9744; implement branching and merging coinswaps for takers, so that they can create coinswaps even if they just have one UTXO
* &#9744; add encrypted wallet files
* &#9744; reproducible builds + pin dependencies to a hash
* &#9744; break as many blockchain analysis heuristics as possible, e.g. change address detection
* &#9744; create a GUI for taker
* &#9744; find coins landing on already-used addresses and freeze them, to resist the [forced address reuse attack](https://en.bitcoin.it/wiki/Privacy#Forced_address_reuse)
* &#9744; payjoin-with-coinswap with decoy UTXOs
* &#9744; convert contracts which currently use script to instead use adaptor signatures, aiming to not reveal contracts in the backout case
* &#9744; create a [web API](https://github.com/JoinMarket-Org/joinmarket-clientserver/blob/master/docs/JSON-RPC-API-using-jmwalletd.md) similar to the [one in joinmarket](https://github.com/JoinMarket-Org/joinmarket-clientserver/issues/978)
* &#9744; randomized locktimes, study with bayesian inference the best way to randomize them so that an individual maker learns as little information as possible from the locktime value
* &#9744; anti-DOS protocol additions for maker (not using json but some kind of binary format that is harder to DOS)
* &#9744; abstract away the Core RPC so that its functions can be done in another way, for example for the taker being supported as a plugin for electrum
* &#9744; make the project into a plugin which can be used by other wallets to do the taker role, try to implement it for electrum wallet

## Community

* IRC channel: `#coinswap`. Logs available [here](http://gnusha.org/coinswap/). Accessible on the [libera IRC network](https://libera.chat/) at `irc.libera.chat:6697 (TLS)` and on the [webchat client](https://web.libera.chat/#coinswap). Accessible anonymously to Tor users on the [Hackint network](https://www.hackint.org/transport/tor) at `ncwkrwxpq2ikcngxq3dy2xctuheniggtqeibvgofixpzvrwpa77tozqd.onion:6667`.

* Chris Belcher's work diary: https://gist.github.com/chris-belcher/ca5051285c6f8d38693fd127575be44d


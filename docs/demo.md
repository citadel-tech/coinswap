# Coinswap Live Demo – Prerequisites & Setup

This guide helps you prepare your system to participate in the Coinswap Live Demo. Follow the steps below to ensure a smooth setup.

---

##  System Prerequisites

### 1. Install Required Software

####  Rust & Cargo
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```
Verify:
```bash
rustc --version
cargo --version
```

---

####  Bitcoin Core
```bash
# Download Bitcoin Core 28.1
wget https://bitcoincore.org/bin/bitcoin-core-28.1/bitcoin-28.1-x86_64-linux-gnu.tar.gz

# Download and verify signatures
wget https://bitcoincore.org/bin/bitcoin-core-28.1/SHA256SUMS
wget https://bitcoincore.org/bin/bitcoin-core-28.1/SHA256SUMS.asc

# Verify the binary
sha256sum --check SHA256SUMS --ignore-missing

# Extract and install
tar xzf bitcoin-28.1-x86_64-linux-gnu.tar.gz
sudo install -m 0755 -o root -g root -t /usr/local/bin bitcoin-28.1/bin/*
```

Verify:
```bash
bitcoind --version
```

---

####  Build Dependencies
```bash
sudo apt-get update
sudo apt install build-essential automake libtool
```

---

####  Setup Tor
Tor is required for all Coinswap communication.

- See [Tor Setup Guide](tor.md).
- Sample `torrc`:
```ini
ControlPort 9051
CookieAuthentication 0
SOCKSPort 9050
```

---

##  Bitcoin Core Setup

### 1. Create Configuration File
```bash
mkdir -p ~/.bitcoin
```

Add to `~/.bitcoin/bitcoin.conf`:
```ini
regtest=1

[regtest]
server=1
txindex=1
rpcuser=user
rpcpassword=password
fallbackfee=0.00001000
blockfilterindex=1
addnode=172.81.178.3:18444
```

> ℹ️ Change `regtest=1` to `testnet4=1` to run on testnet.

---

### 2. Start Bitcoin Core
```bash
bitcoind
```

Wait for IBD (Initial Block Download). Verify:
```bash
bitcoin-cli getblockchaininfo
```

---

##  Compile the Coinswap Apps

```bash
git clone https://github.com/citadel-tech/coinswap.git
cd coinswap
cargo build --release
```

After build, install binaries:
```bash
sudo install ./target/release/taker /usr/local/bin/
sudo install ./target/release/makerd /usr/local/bin/
sudo install ./target/release/maker-cli /usr/local/bin/
```

---

##  Run the Swap Server

`makerd` runs the server. `maker-cli` controls it.

### Start `makerd`
```bash
makerd
```

Follow logs. You'll be asked to send BTC to a generated address (min: **20,000 sats**).

Use [mempool.space Testnet4 faucet](https://mempool.space/testnet4/faucet) or another to fund.

Once funded and confirmed, the server will:

- Create a fidelity bond
- Broadcast offers to the DNS server
- Listen for swaps

### Use `maker-cli`
```bash
maker-cli --help
maker-cli get-balances
maker-cli list-utxo
```

>  Wallet & data at: `~/.coinswap/maker/`. Backup `wallets/maker-wallet`.

---

##  Run the Swap Client

Run from a separate terminal with the `taker` app.

### 1. Get an Address & Fund It
```bash
taker get-new-address
```

Fund via a faucet, then:
```bash
taker get-balances
```

### 2. Fetch Offers
```bash
taker fetch-offers
```

### 3. Attempt Coinswap
```bash
taker coinswap
```

---

##  Troubleshooting

### Bitcoin Core
- Is it running? → `bitcoin-cli getblockchaininfo`
- Are RPC credentials matching?
- Are you on the right network?

### Maker Server
- Check `debug.log`
- Was fidelity bond created?
- Are funds sufficient?
- Is Tor connected?

### Taker Client
- Is the wallet funded?
- Check logs and network status

### Still stuck?
Join our [Discord](https://discord.gg/gs5R6pmAbR) for support.

---

##  Additional Resources

- [Bitcoin Core Docs](https://bitcoin.org/en/developer-reference)
- [Coinswap Protocol Spec](https://github.com/citadel-tech/Coinswap-Protocol-Specification)
- [Project Repo](https://github.com/citadel-tech/coinswap)

### Related Guides:
- [Bitcoin Core Setup](./bitcoind.md)
- [Maker Server Guide](./makerd.md)
- [Maker CLI Reference](./maker-cli.md)
- [Taker Client Guide](./taker.md)

---
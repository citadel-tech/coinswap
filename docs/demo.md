# Coinswap Live Demo – Updated Prerequisites and Setup Guide

This guide will help you prepare your system for participating in the Coinswap Live Demo. Follow these steps carefully to ensure a smooth experience during the demonstration.

---

##  System Requirements

### Required Software

1. **Rust and Cargo**
   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   source $HOME/.cargo/env
   ```

   Verify installation:
   ```bash
   rustc --version
   cargo --version
   ```

2. **Bitcoin Core (v28.1)**
   ```bash
   wget https://bitcoincore.org/bin/bitcoin-core-28.1/bitcoin-28.1-x86_64-linux-gnu.tar.gz
   wget https://bitcoincore.org/bin/bitcoin-core-28.1/SHA256SUMS
   wget https://bitcoincore.org/bin/bitcoin-core-28.1/SHA256SUMS.asc
   sha256sum --check SHA256SUMS --ignore-missing

   tar xzf bitcoin-28.1-x86_64-linux-gnu.tar.gz
   sudo install -m 0755 -o root -g root -t /usr/local/bin bitcoin-28.1/bin/*
   ```

   Verify:
   ```bash
   bitcoind --version
   ```

3. **Build Dependencies**
   ```bash
   sudo apt update
   sudo apt install -y build-essential automake libtool pkg-config
   ```

4. **Tor (for anonymity & networking)**

   Coinswap uses Tor for all communications.

   **Quick `torrc` config** (usually at `/etc/tor/torrc` or `~/.tor/torrc`):
   ```conf
   ControlPort 9051
   CookieAuthentication 0
   SOCKSPort 9050
   ```

   For complete setup, refer to [tor.md](./tor.md)

---

##  Bitcoin Core Setup

### Step 1: Configuration File
```bash
mkdir -p ~/.bitcoin
nano ~/.bitcoin/bitcoin.conf
```

Paste:
```conf
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

> To run on testnet4, change `regtest=1` to `testnet4=1`.

### Step 2: Start the Bitcoin Node
```bash
bitcoind
```

Check sync status:
```bash
bitcoin-cli getblockchaininfo
```

---

##  Compile Coinswap Apps

```bash
git clone https://github.com/citadel-tech/coinswap.git
cd coinswap
cargo build --release
```

Install binaries:
```bash
sudo install ./target/release/taker /usr/local/bin/
sudo install ./target/release/makerd /usr/local/bin/
sudo install ./target/release/maker-cli /usr/local/bin/
```

---

##  Running the Maker Server

The server consists of two apps: `makerd` (daemon) and `maker-cli` (RPC client).

Start daemon:
```bash
makerd
```

Once logs appear, you’ll see instructions for funding and setting up a fidelity bond.

###  Fund the Maker
- Wait for `makerd` to show a funding address.
- Use [mempool.space testnet4 faucet](https://mempool.space/testnet4/faucet) to send **≥ 20,000 sats**.

Maker will:
- Auto-create a fidelity bond
- Wait for confirmation
- Publish offers to the DNS
- Start accepting swaps

###  Use `maker-cli`
Try:
```bash
maker-cli get-balances
maker-cli list-utxo
maker-cli get-offers
```

> **Note**: All data is stored at `~/.coinswap/maker/`. Back up the wallet:
```bash
cp ~/.coinswap/maker/wallets/maker-wallet ./maker-wallet.backup
```

---

##  Run the Taker Client

From a separate terminal:

### Step 1: Get a Receiving Address
```bash
taker get-new-address
```

Fund it using the testnet4 faucet.

### Step 2: Check Balance
```bash
taker get-balances
```

### Step 3: Fetch Market Offers
```bash
taker fetch-offers
```

### Step 4: Perform a Coinswap
```bash
taker coinswap
```

You’ll see the swap progress in logs.

---

##  Troubleshooting Tips

### Bitcoin Core
- `bitcoin-cli getblockchaininfo`
- Match `rpcuser/password` in `bitcoin.conf`
- Ensure correct network (regtest/testnet4)

### Maker Issues
- Check logs: `~/.coinswap/maker/debug.log`
- Confirm fidelity bond creation
- Ensure funding address has ≥ 20,000 sats
- Verify Tor is running

### Taker Issues
- Ensure wallet is funded
- Watch logs: `~/.coinswap/taker/debug.log`
- Confirm network connectivity

---

##  Need Help?

Still stuck?

- Join the [Discord Support Server](https://discord.gg/gs5R6pmAbR)
- Tag developers or ask in #coinswap-support

---

##  Additional Resources

- [Bitcoin Core Docs](https://bitcoin.org/en/developer-reference)
- [Coinswap Protocol Spec](https://github.com/citadel-tech/Coinswap-Protocol-Specification)
- [Coinswap GitHub Repo](https://github.com/citadel-tech/coinswap)

Component Guides:
- [Bitcoin Core Setup](./bitcoind.md)
- [Maker Server](./makerd.md)
- [Maker CLI](./maker-cli.md)
- [Taker Client](./taker.md)
- [Tor Setup](./tor.md)
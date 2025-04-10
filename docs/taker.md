# Taker Tutorial

The **Taker** is the party that initiates a CoinSwap. It connects to the Directory Server, fetches available Maker offers, selects suitable ones, and coordinates the swap.

This guide walks you through setting up and using the Taker CLI to perform a CoinSwap on **testnet4**.

---

## Prerequisites

Before starting, ensure:

- You have **bitcoind** running with RPC enabled on `testnet4`.
- You have some **testnet4 coins** (via faucet).
- Tor is set up (for `.onion` access to Directory Server).

>  All components are designed to run on **testnet4**. The Taker will not function properly on other networks without custom DNS coordination.

---

## Start Bitcoin Core

```bash
bitcoind -testnet -daemon
```

Check it’s running:

```bash
bitcoin-cli -testnet getblockchaininfo
```

If `bitcoind` isn't installed, refer to the [bitcoind demo](./bitcoind.md).

---

## Running Taker CLI

To view all commands:

```bash
./taker --help
```

Example:

```bash
taker 0.1.0
A command-line client for initiating CoinSwaps

USAGE:
    taker [OPTIONS] <SUBCOMMAND>

OPTIONS:
    -a, --USER:PASSWORD     Bitcoin RPC auth (e.g., user:pass)
    -r, --ADDRESS:PORT      RPC address (default: 127.0.0.1:18443)
    -d, --data-directory    Custom data dir (default: ~/.coinswap/taker)
    -w, --WALLET            Wallet name (default: taker-wallet)
    -v, --verbosity         Log level (default: info)
    -V, --version           Show version
    -h, --help              Show help

SUBCOMMANDS:
    get-new-address         Generates a new receiving address
    get-balances            Shows wallet balances
    list-utxo               Shows spendable UTXOs
    list-utxo-contract      Shows contract UTXOs (HTLC)
    list-utxo-swap          Shows UTXOs from swaps
    fetch-offers            Sync and display current Maker offers
    do-coinswap             Initiates a CoinSwap
    send-to-address         Sends funds to another address
```

---

## Step-by-Step Demo

### 1. Get a New Address

```bash
./taker -r 127.0.0.1:38332 -a user:pass get-new-address
```

Example output:

```
bcrt1qyywgd4we5y7u05lnrgs8runc3j7sspwqhekrdd
```

### 2. Fund Your Wallet

Send coins from a testnet4 faucet: [https://mempool.space/testnet4/faucet](https://mempool.space/testnet4/faucet)

### 3. Check Balances

```bash
./taker -r 127.0.0.1:38332 -a user:pass get-balances
```

Example output:

```json
{
    "regular": 10000000,
    "swap": 0,
    "contract": 0,
    "spendable": 10000000
}
```

### 4. Fetch Available Offers

```bash
./taker -r 127.0.0.1:38332 -a user:pass fetch-offers
```

This updates the offer book from the Directory Server.

### 5. Start a CoinSwap

```bash
./taker -r 127.0.0.1:38332 -a user:pass do-coinswap
```

This starts a CoinSwap using fetched maker offers. Follow the terminal prompts.

To monitor progress:

```bash
tail -f ~/.coinswap/taker/debug.log
```

---

## Data, Config, and Wallets

By default, Taker saves everything to `~/.coinswap/taker`. You can override with `--data-directory`.

### Folder Structure:

- `config.toml`: Taker configuration
- `debug.log`: Logs
- `wallets/`: Wallet storage

### Example `config.toml`

```toml
control_port = 9051
socks_port = 9050
tor_auth_password = ""
directory_server_address = "ri3t5m2na2eestaigqtxm3f4u7njy65aunxeh7aftgid3bdeo3bz65qd.onion:8080"
connection_type = "TOR"
```

#### Fields:

- **control_port**: Tor Control Port (see [tor.md](./tor.md))
- **socks_port**: Tor SOCKS Port
- **tor_auth_password**: Optional password for Tor auth
- **directory_server_address**: Onion address of Directory Server
- **connection_type**: `TOR` or `CLEARNET`

---

## Backup Notice

Taker wallet files are stored in `wallets/`. These contain your **private keys** — make sure to back them up securely.
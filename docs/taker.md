# Taker Tutorial

The taker is the party that initiates the coinswap. It queries the directory server for a list of makers, requests offers from them and selects suitable makers for the swap. It then conducts the swap with the selected makers.

This guide walks you through setting up and using the Taker CLI to perform a CoinSwap on **testnet4**.

---

## Prerequisites

Before starting, ensure:

- You have **bitcoind** running with RPC enabled on `testnet4`.
- You have some **testnet4 coins** (you can get them via a faucet).
- Tor is set up and running to access the `.onion` Directory Server.

> All components are designed to run on **testnet4**. The Taker will not function properly on other networks without custom DNS coordination.

---

## Start Bitcoin Core

Run the following command to start `bitcoind` on `testnet4`:

```bash
bitcoind -testnet -daemon
```

Verify it's running:

```bash
bitcoin-cli -testnet getblockchaininfo
```

If you don’t have `bitcoind` installed, refer to the [bitcoind demo](./bitcoind.md) for setup instructions.

---

## Running the Taker CLI

To view all available commands:

```bash
./taker --help
```

Example output:

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

This section will guide you through a basic CoinSwap using the CLI.

### 1. Get a New Address

This command creates a new receiving address for your wallet:

```bash
./taker -r 127.0.0.1:38332 -a user:pass get-new-address
```

Example output:

```
bcrt1qyywgd4we5y7u05lnrgs8runc3j7sspwqhekrdd
```

---

### 2. Fund Your Wallet

Send some testnet coins to the address above using a faucet such as:

 [https://mempool.space/testnet4/faucet](https://mempool.space/testnet4/faucet)

---

### 3. Check Your Wallet Balance

After funding, check your wallet’s balance:

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

- `regular`: Funds in the wallet
- `swap`: Funds involved in ongoing swaps
- `contract`: HTLCs (Hashed Timelock Contracts)
- `spendable`: Available balance for spending

---

### 4. Fetch Available Maker Offers

Retrieve current offers from the Directory Server:

```bash
./taker -r 127.0.0.1:38332 -a user:pass fetch-offers
```

This command connects to the Directory Server over Tor, downloads the latest Maker offers, and displays them.

---

### 5. Start a CoinSwap

Now that you’ve fetched offers, initiate a CoinSwap:

```bash
./taker -r 127.0.0.1:38332 -a user:pass do-coinswap
```

This will match with suitable Makers and guide you through the swap process via CLI prompts.

To monitor logs in real time:

```bash
tail -f ~/.coinswap/taker/debug.log
```

---

## Data, Configuration, and Wallet Files

By default, all Taker-related data is stored in:

```
~/.coinswap/taker
```

You can override this with the `--data-directory` option.

### Folder Structure

- `config.toml`: Taker configuration file
- `debug.log`: CLI logs for debugging
- `wallets/`: Directory where wallet files are stored

---

### Example `config.toml`

```toml
control_port = 9051
socks_port = 9050
tor_auth_password = ""
directory_server_address = "ri3t5m2na2eestaigqtxm3f4u7njy65aunxeh7aftgid3bdeo3bz65qd.onion:8080"
connection_type = "TOR"
```

#### Field Descriptions:

- **control_port**: Port used by Tor for control commands (see [tor.md](./tor.md))
- **socks_port**: Port used by Tor SOCKS proxy
- **tor_auth_password**: Optional authentication for Tor control
- **directory_server_address**: `.onion` address of the Directory Server
- **connection_type**: Connection method; can be `TOR` or `CLEARNET`

---

## Backup Notice

Taker wallet files are saved in the `wallets/` directory. These files contain your **private keys** and must be backed up securely. Loss of this directory means loss of access to your funds.

---

## Additional Notes

- Make sure your Tor service is running before fetching offers or initiating swaps.
- Use the debug log to troubleshoot issues.
- CoinSwaps may take time depending on network and Maker responsiveness.
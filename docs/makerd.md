# Maker Overview

The **Maker** provides liquidity for a coin swap initiated by a **Taker**. In return, the Maker earns fees for facilitating the swap.

This component consists of two key tools:

- **`makerd`** – A background daemon that runs the Maker server.
- **`maker-cli`** – A CLI tool to interact with `makerd` via RPC.

The `makerd` daemon handles:
- Coinswap protocol messaging
- DNS interactions
- Fidelity bond processing
- Taker coordination

>  **Warning:**  
> Maker private keys reside in a hot wallet managed by `makerd`. Ensure the server is secure and access-controlled.

---

## File Storage and Configuration

All Maker data is stored in:  
`$HOME/.coinswap/maker`

### `config.toml` (Default Configuration)

```toml
network_port = 6102
rpc_port = 6103
socks_port = 9050
control_port = 9051
tor_auth_password = ""
min_swap_amount = 10000
fidelity_amount = 50000
fidelity_timelock = 13104
connection_type = TOR
directory_server_address = ri3t5m2na2eestaigqtxm3f4u7njy65aunxeh7aftgid3bdeo3bz65qd.onion:8080
```

#### Key Fields:
- `network_port`: Port for Coinswap messages
- `rpc_port`: RPC access port for `maker-cli`
- `socks_port`: Tor SOCKS port (see [Tor doc](./tor.md))
- `control_port`: Tor Control Port (see [Tor doc](./tor.md))
- `tor_auth_password`: Optional Tor auth password
- `min_swap_amount`: Minimum swap value (in sats)
- `fidelity_amount`: Locked bond (in sats)
- `fidelity_timelock`: Lock duration (in blocks)
- `connection_type`: Network type (currently hardcoded to `TOR`)
- `directory_server_address`: DNS server address

>  **Note:**  
> Coinswap currently supports only the **TOR** network. `CLEARNET` is not supported in production.

---

### Wallets Directory

Stores the Maker's wallet files (private keys, metadata).  
Path: `$HOME/.coinswap/maker/wallets`

Make sure to securely back up wallet data.

---

### Log File: `debug.log`

Contains logs for troubleshooting and monitoring `makerd`.

---

## Maker Setup Tutorial

This tutorial helps you run and manage the `makerd` server.

### Prerequisites

Start `bitcoind` on **testnet4**:

```bash
$ bitcoind
```

> See the [bitcoind setup guide](./bitcoind.md) if needed.

---

### Check Available Options

```bash
$ ./makerd --help
```

**Output Sample:**

```
coinswap 0.1.0
Coinswap Maker Server - Citadel-Tech

USAGE:
  makerd [OPTIONS]

OPTIONS:
  -a, --USER:PASSWD     Bitcoin Core RPC credentials [default: user:password]
  -d, --data-directory  Data directory [default: ~/.coinswap/maker]
  -r, --ADDRESS:PORT    Bitcoin Core RPC host:port [default: 127.0.0.1:18443]
  -w, --WALLET          Wallet name [default: maker-wallet]
  -h, --help            Show help
  -V, --version         Show version
```

---

## Starting `makerd`

```bash
$ ./makerd --USER:PASSWD user:password --ADDRESS:PORT 127.0.0.1:18443
```

### What Happens Next:

- **Wallet Setup**
  - If wallet is missing, `makerd` creates a new one:
    ```bash
    INFO - Backup the Wallet Mnemonics: ["..."]
    INFO - New Wallet created at ~/.coinswap/maker/wallets/maker-wallet
    ```

- **Config Setup**
  - If missing, `makerd` creates default config:
    ```bash
    WARN - Maker config file not found, creating...
    INFO - Loaded config from ~/.coinswap/maker/config.toml
    ```

- **Wallet Sync**
  ```bash
  INFO - Initializing wallet sync
  INFO - Completed wallet sync
  ```

- **TOR Bootstrap**
  ```bash
  INFO - Server listening on TOR: 3xvc6tvf...onion:6102
  ```

- **Fidelity Bond Creation**
  ```bash
  INFO - No active bonds found. Creating one...
  INFO - Fidelity value = 0.0005 BTC, Fee = 1000 sats
  ```

>  Fee is currently hardcoded. Will be improved in v0.1.1.

---

### Fund the Wallet

To create a fidelity bond, fund your wallet with testnet coins (~0.00051000 BTC minimum).

Use a [testnet4 faucet](https://mempool.space/testnet4/faucet).  
Recommend: **0.01 BTC** (extra for transactions in [maker-cli](./maker-cli.md))

---

### Auto-Detect & Confirm Funding

Once funds are received:

```bash
INFO - Fidelity Transaction <txid> seen in mempool
INFO - Fidelity Transaction <txid> confirmed at blockheight: 229349
```

The bond is now active.

---

### Register Maker on DNS

```bash
INFO - Successfully sent our address to dns at <dns_address>
```

---

### Background Threads Spawned

```bash
INFO - Spawning Bitcoin Core checker
INFO - Spawning Client connection checker
INFO - Spawning contract-watcher
INFO - Spawning RPC server (bound at 127.0.0.1:6103)
```

---

##  Setup Complete

```bash
INFO - Server Setup completed!! Use maker-cli to operate the server and wallet.
```

You can now operate your Maker via the `maker-cli` tool.

>  Continue to the [maker-cli demo](./maker-cli.md) for wallet management, swap inspection, and other commands.
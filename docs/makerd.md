## Maker Overview

The **Maker** is the party that provides liquidity for a coin swap initiated by a **Taker**. In return, the Maker earns a fee for facilitating the swap.

The Maker component is based on the `makerd/maker-cli` architecture, which is similar to `bitcoind/bitcoin-cli`. The `makerd` is a background daemon that handles the heavy tasks in the **Coinswap** protocol, such as interacting with the **DNS** system, maintaining fidelity bonds, and processing Taker requests.

The `makerd` server should run 24/7 to ensure it can process Taker requests and facilitate coin swaps at any time.

> **Warning:**  
> Maker private keys should be kept in a hot wallet used by `makerd` to facilitate coin swap requests. Users are responsible for securing the server-side infrastructure.

The `maker-cli` is a command-line application that allows you to operate and manage `makerd` through RPC commands.

## Data, Configuration, and Wallets

Maker stores all its data in a directory located by default at `$HOME/.coinswap/maker`. This directory contains the following important files:

**Default Maker Configuration (`~/.coinswap/maker/config.toml`):**

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
- `network_port`: TCP port where the Maker listens for incoming Coinswap protocol messages.
- `rpc_port`: The port through which `makerd` listens for RPC commands from `maker-cli`.
- `socks_port`: The Tor Socks Port.  Check the [tor doc](tor.md) for more details.
- `control_port`: The Tor Control Port. Check the [tor doc](tor.md) for more details.
- `tor_auth_password`: Optional password for Tor control authentication; empty by default.
- `min_swap_amount`: Minimum swap amount in satoshis.
- `fidelity_amount`: Amount in satoshis locked as a fidelity bond to deter Sybil attacks.
- `fidelity_timelock`: Lock duration in block heights for the fidelity bond.
- `connection_type`: Specifies the network mode; set to "TOR" in production for privacy, or "CLEARNET" during testing.
- `directory_server_address`: The Tor address of the DNS Server. This value is set to a fixed default for now.



> **Important:**  
> At the moment, Coinswap operates only on the **TOR** network. The `connection_type` is hardcoded to `TOR`, and the app will only work with this network until multi-network support is added.

### 2. **wallets Directory**

This folder contains the wallet files used by the Maker to store wallet data, including private keys. Ensure these wallet files are backed up securely.

The default wallet directory is `$HOME/.coinswap/maker/wallets`.

### 3. **debug.log**

The log file for `makerd`, where debug information is stored for troubleshooting and monitoring.

---

## Maker Tutorial

In this tutorial, we will guide you through the process of operating the Maker component, including how to set up `Makerd` and how to use `maker-cli` for managing `Makerd` and performing wallet-related operations.

This tutorial is split into two parts:

- **Makerd Tutorial**
- **maker-cli Tutorial**

This section focuses on `Makerd`, walking you through the process of starting and fully setting up the server. For instructions on `maker-cli`, refer to the [maker-cli demo](./maker-cli.md).

---

## How to Set Up Makerd

### 1. Start Bitcoin Core (Pre-requisite)

`Makerd` requires a **Bitcoin Core** RPC connection running on **testnet4** for its operation. To get started, you need to start `bitcoind`:

> **Important:**  
> All apps are designed to run on **testnet4** for testing purposes. The DNS server that Maker connects to will also be on testnet4. While you can run these apps on other networks, there won't be any DNS available, so Maker won’t be able to connect to the DNS server or other Coinswap networks.

To start `bitcoind`:

```bash
$ bitcoind
```

**Note:** If you don’t have `bitcoind` installed or need help setting it up, refer to the [bitcoind demo documentation](./bitcoind.md).

### 2. Run the Help Command to See All Makerd Arguments

To see all the available arguments for `Makerd`, run the following command:

```bash
$ ./makerd --help
```

This will display information about the `makerd` binary and its options.

**Output:**

```bash
coinswap 0.1.0
Developers at Citadel-Tech
Coinswap Maker Server

The server requires a Bitcoin Core RPC connection running in testnet4. It requires some starting balance (0.05 BTC Fidelity + Swap Liquidity). After the successful creation of a Fidelity Bond, the server will start listening for incoming swap requests and earn swap fees.

The server is operated with the maker-cli app, for all basic wallet-related operations.

For more detailed usage information, please refer: [maker demo doc link]

This is early beta, and there are known and unknown bugs. Please report issues at:
https://github.com/citadel-tech/coinswap/issues

USAGE:
    makerd [OPTIONS]

OPTIONS:
    -a, --USER:PASSWD <USER:PASSWD>
            Bitcoin Core RPC authentication string (username, password)

            [default: user:password]

    -d, --data-directory <DATA_DIRECTORY>
            Optional DNS data directory. Default value: "~/.coinswap/maker"

    -h, --help
            Print help information

    -r, --ADDRESS:PORT <ADDRESS:PORT>
            Bitcoin Core RPC network address

            [default: 127.0.0.1:18443]

    -V, --version
            Print version information

    -w, --WALLET <WALLET>
            Optional wallet name. If the wallet exists, load the wallet, else create a new wallet with the given name. Default: maker-wallet
```

This will give you detailed information about the options and arguments available for `Makerd`.

### Start `makerd`:

To start `makerd`, run the following command:

```bash
./makerd --USER:PASSWD <username>:<password> --ADDRESS:PORT 127.0.0.1:<bitcoind rpc port>
```

This will launch `makerd` and connect it to the Bitcoin RPC core running on it's rpc port, using the default data directory for `maker` located at `$HOME/.coinswap/maker`.


**What happens next:**

- If no wallet file is found at `$HOME/.coinswap/maker/wallets`, `makerd` will create a new wallet named `maker-wallet`.

  ```bash
  INFO coinswap::wallet::api - Backup the Wallet Mnemonics.
  ["harvest", "trust", "catalog", "degree", "oxygen", "business", "crawl", "enemy", "hamster", "music", "this", "idle"]
  
  INFO coinswap::maker::api - New Wallet created at: "$HOME/.coinswap/maker/wallets/maker-wallet"
  ```

- If no `config` file exists, `makerd` will create a default `config.toml` file at `$HOME/.coinswap/maker/config.toml`.

   ```bash
   WARN coinswap::maker::config - Maker config file not found, creating default config file at path: /tmp/coinswap/maker/config.toml
   INFO coinswap::maker::config - Successfully loaded config file from: $HOME/.coinswap/maker/config.toml
   ```

- The wallet will sync to catch up with the latest updates.

  ```bash
  INFO coinswap::maker::api - Initializing wallet sync
  INFO coinswap::maker::api - Completed wallet sync
  ```

- `makerd` will start the TOR process and listen for connections on a TOR address.

  ```bash
  INFO coinswap::maker::server - [6102] Server is listening at 3xvc6tvf455afnogiwhzpztp7r5w43kq4r2yb5oootu7rog6k6rnq4id.onion:6102
  ```

- `makerd` checks for existing fidelity bonds. If none are found, it will create one using the fidelity amount and timelock from the configuration file. By default, the fidelity amount is `50,000 sats` and the timelock is `2160 blocks`.

  ```bash
  INFO coinswap::maker::server - No active Fidelity Bonds found. Creating one.
  INFO coinswap::maker::server - Fidelity value chosen = 0.0005 BTC
  INFO coinswap::maker::server - Fidelity Tx fee = 1000 sats
  ```

  > **Note**: Currently The transaction fee for the fidelity bond is hardcoded at `1000 sats`. This approach of directly considering `fee` not `fee rate` will be improved in v0.1.1 milestones.

- Since the maker wallet is empty, we'll need to fund it with at least `0.00051000 BTC` to cover the fidelity amount and transaction fee. To fund the wallet, we can use a testnet4 faucet from [testnet4 Faucets](https://mempool.space/testnet4/faucet).
  Let's just take `0.01 BTC`testcoins as extra amount will be used in doing wallet related operations in [maker-cli demo](./maker-cli.md)

- The server will regularly sync the wallet every 10 seconds, increasing the interval in the pattern 10,20,30,40..., to detect any incoming funds.

- Once the server detects a funding transaction, it will automatically create and broadcast a fidelity transaction using the funding UTXOs.

  ```bash
  INFO coinswap::wallet::fidelity - Fidelity Transaction 4593a892809621b64418d6bf9590c6536a1fa27f7a136d176ad302fb8ec3ce23 seen in mempool, waiting for confirmation.
  ```
  
- Once the transaction is confirmed:
  
  ```bash
  INFO coinswap::wallet::fidelity - Fidelity Transaction 4593a892809621b64418d6bf9590c6536a1fa27f7a136d176ad302fb8ec3ce23 confirmed at blockheight: 229349
  INFO coinswap::maker::server - [6102] Successfully created fidelity bond
  ```

- After the fidelity bond is created, `makerd` will send its address to the DNS address book.

  ```bash
  INFO coinswap::maker::server - [6102] Successfully sent our address to dns at <dns_address>
  ```

- Several threads will now be spawned to handle specific tasks:

  ```bash
  INFO coinswap::maker::server - [6102] Spawning Bitcoin Core connection checker thread
  INFO coinswap::maker::server - [6102] Spawning Client connection status checker thread
  INFO coinswap::maker::server - [6102] Spawning contract-watcher thread
  INFO coinswap::maker::server - [6102] Spawning RPC server thread
  INFO coinswap::maker::rpc::server - [6102] RPC socket binding successful at 127.0.0.1:6103
  ```

 Finally, the `makerd` server is fully set up and ready to connect with other takers for coin swaps. Once everything is initialized, you can use the `maker-cli` to interact with the server, manage its wallet, and perform various operations.

```bash
INFO coinswap::maker::server - [6102] Server Setup completed!! Use maker-cli to operate the server and the internal wallet.
```

---

For detailed instructions on how to use the maker-cli, please refer to the [maker-cli demo](./maker-cli.md) . This guide will provide a comprehensive overview of the available commands and features for operating your maker server effectively.

---

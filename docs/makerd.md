## Maker Overview

The **Maker** is the party that provides liquidity for a coin swap initiated by a **Taker**. In return, the Maker earns a fee for facilitating the swap.

The Maker component is based on the `makerd/maker-cli` architecture, which is similar to `bitcoind/bitcoin-cli`. The `makerd` is a background daemon that handles the heavy tasks in the **Coinswap** protocol, such as maintaining fidelity bonds, and processing Taker requests.

The `makerd` server should run 24/7 to ensure it can process Taker requests and facilitate coin swaps at any time.

> **Warning:**  
> Maker private keys should be kept in a hot wallet used by `makerd` to facilitate coin swap requests. Users are responsible for securing the server-side infrastructure.

The `maker-cli` is a command-line application that allows you to operate and manage `makerd` through RPC commands.

## Data, Configuration, and Wallets

Maker stores all its data in a directory located by default at `$HOME/.coinswap/maker`. This directory contains the following important files:

### **1. Default Maker Configuration (`~/.coinswap/maker/config.toml`):**

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
base_fee = 100,
amount_relative_fee_pct = 0.1,
```
- `network_port`: TCP port where the Maker listens for incoming Coinswap protocol messages.
- `rpc_port`: The port through which `makerd` listens for RPC commands from `maker-cli`.
- `socks_port`: The Tor Socks Port.  Check the [tor doc](tor.md) for more details.
- `control_port`: The Tor Control Port. Check the [tor doc](tor.md) for more details.
- `tor_auth_password`: Optional password for Tor control authentication; empty by default.
- `min_swap_amount`: Minimum swap amount (in satoshis).
- `fidelity_amount`: Amount (in satoshis) locked as a fidelity bond to deter Sybil attacks.
- `fidelity_timelock`: Lock duration in block heights for the fidelity bond.
- `connection_type`: Specifies the network mode; set to "TOR" in production for privacy, or "CLEARNET" during testing.
- `base_fee`: A fixed fee charged by the Maker for providing its services (in satoshis).
- `amount_relative_fee_pct`: A percentage fee based on the swap amount.



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

`Makerd` requires a **Bitcoin Core** RPC connection running on **signet** for its operation (check [demo doc](./demo.md)). To get started, you need to start `bitcoind`:

> **Important:**  
> All apps are designed to run on our **custom signet** for testing purposes. The marketplace is only live in custom signet. Running the maker in other networks will not work as there's no marketplace in that network.

To start `bitcoind`:

```bash
$ bitcoind
```

**Note:** If you don't have `bitcoind` installed or need help setting it up, refer to the [bitcoind demo documentation](./bitcoind.md).

### 2. Run the Help Command to See All Makerd Arguments

To see all the available arguments for `Makerd`, run the following command:

```bash
$ ./makerd --help
```

This will display information about the `makerd` binary and its options.

**Output:**

```bash
coinswap 0.1.2
Developers at Citadel-Tech
Coinswap Maker Server

The server requires a Bitcoin Core RPC connection running in Testnet4. It requires some starting
balance, around 50,000 sats for Fidelity + Swap Liquidity (suggested 50,000 sats). So topup with at
least 0.001 BTC to start all the node processses. Suggested [faucet
here] http://xjw3jlepdy35ydwpjuptdbu3y74gyeagcjbuyq7xals2njjxrze6kxid.onion/

All server processes will start after the fidelity bond transaction is confirmed. This may take some
time. Approx: 10 mins. Once the bond is confirmed, the server starts listening for incoming swap
requests. As it performs swaps for clients, it keeps earning fees.

The server is operated with the maker-cli app, for all basic wallet related operations.

For more detailed usage information, please refer the [Maker
Doc] https://github.com/citadel-tech/coinswap/blob/master/docs/makerd.md

This is early beta, and there are known and unknown bugs. Please report issues in the [Project Issue
Board] https://github.com/citadel-tech/coinswap/issues

USAGE:
    makerd [OPTIONS]

OPTIONS:
    -a, --USER:PASSWORD <USER:PASSWORD>
            Bitcoin Core RPC authentication string (username, password)
            
            [default: user:password]

    -d, --data-directory <DATA_DIRECTORY>
            Optional data directory. Default value: "~/.coinswap/maker"

    -h, --help
            Print help information

    -r, --ADDRESS:PORT <ADDRESS:PORT>
            Bitcoin Core RPC network address
            
            [default: 127.0.0.1:38332]

    -t, --tor-auth <TOR_AUTH>
            [default: ]

    -V, --version
            Print version information

    -w, --WALLET <WALLET>
            Optional wallet name. If the wallet exists, load the wallet, else create a new wallet
            with the given name. Default: maker-wallet
```

This will give you detailed information about the options and arguments available for `Makerd`.

### Key Points About Command Arguments

- The `-r` or `--ADDRESS:PORT` option specifies the Bitcoin Core RPC address and port. By default, this is set to **`127.0.0.1:38332`**.

- The `-a` or `--USER:PASSWORD` option specifies the Bitcoin Core RPC authentication. By default, this is set to **`user:password`**.

- #### If you're using the **default configuration**:

  - You don't need to include these arguments.

- #### If you're using a **custom configuration**:
  - Pass your custom values using the `-r` and `-a` options, like this:

```bash
  $ ./makerd -r 127.0.0.1:38332 -a myuser:mypass
```

## For this tutorial, we'll assume a custom configuration with port 38332. Output examples will reflect this setup.

### Start `makerd`:

To start `makerd`, run the following command:

```bash
$ ./makerd -a user:password -r 127.0.0.1:38332
```

This will launch `makerd` and connect it to the Bitcoin RPC core running on its rpc port, using the default data directory for `maker` located at `$HOME/.coinswap/maker`.

**What happens next:**

- **Wallet Loading**: If an existing wallet file is found at `$HOME/.coinswap/maker/wallets`, `makerd` will load it:

  ```bash
  INFO coinswap::wallet::api - Wallet file at "/path/to/maker-wallet" successfully loaded.
  ```

- **New Wallet Creation**: If no wallet file is found, `makerd` will create a new wallet named `maker-wallet`:

  ```bash
  INFO coinswap::wallet::api - Backup the Wallet Mnemonics.
  ["harvest", "trust", "catalog", "degree", "oxygen", "business", "crawl", "enemy", "hamster", "music", "this", "idle"]
  
  INFO coinswap::maker::api - New Wallet created at: "$HOME/.coinswap/maker/wallets/maker-wallet".
  ```

- **Configuration File**: If no `config` file exists, `makerd` will create a default `config.toml` file at `$HOME/.coinswap/maker/config.toml`:

   ```bash
   WARN coinswap::maker::config - Maker config file not found, creating default config file at path: /tmp/coinswap/maker/config.toml
   INFO coinswap::maker::config - Successfully loaded config file from: $HOME/.coinswap/maker/config.toml
   ```

- **Wallet Sync**: The wallet will sync to catch up with the latest updates:

  ```bash
  INFO coinswap::wallet::rpc - Initializing wallet sync and save
  INFO coinswap::wallet::rpc - Completed wallet sync and save
  ```

- **TOR Initialization**: `makerd` will start the TOR process and listen for connections on a TOR address:

  ```bash
  INFO coinswap::maker::server - [6102] Server is listening at 3xvc6tvf455afnogiwhzpztp7r5w43kq4r2yb5oootu7rog6k6rnq4id.onion:6102
  ```

- **Fidelity Bond Check**: `makerd` checks for existing fidelity bonds. 

  **If an existing fidelity bond is found**:
  ```bash
  INFO coinswap::wallet::fidelity - Fidelity Bond found | Index: 0 | Bond Value : 0.00043653 BTC
  INFO coinswap::maker::server - Highest bond at outpoint fc11a129...c:0 | Amount 5000000 sats | Remaining Timelock for expiry : 536 Blocks
  ```

  **If no fidelity bonds are found**, it will create one using the fidelity amount and timelock from the configuration file. By default, the fidelity amount is `50,000 sats` and the timelock is `13104 blocks`:

  ```bash
  INFO coinswap::maker::server - No active Fidelity Bonds found. Creating one.
  INFO coinswap::maker::server - Fidelity value chosen = 0.0005 BTC
  INFO coinswap::maker::server - Fidelity Tx fee = 1000 sats
  ```

  > **Note**: Currently the transaction fee for the fidelity bond is hardcoded at `1000 sats`. This approach of directly considering `fee` not `fee rate` will be improved in v0.1.1 milestones.

- **Funding Requirements**: If creating a new fidelity bond and the maker wallet is empty, you'll need to fund it with at least `0.00051000 BTC` to cover the fidelity amount and transaction fee. To fund the wallet, you can use [this faucet](http://s2ncekhezyo2tkwtftti3aiukfpqmxidatjrdqmwie6xnf2dfggyscad.onion/)(open in Tor browser).
  We suggest taking `0.01 BTC` testcoins as the extra amount will be used in doing wallet related operations in [maker-cli demo](./maker-cli.md)

- **Regular Wallet Sync**: The server will regularly sync the wallet every 10 seconds, increasing the interval in the pattern 10,20,30,40..., to detect any incoming funds.

- **Fidelity Transaction Creation**: Once the server detects sufficient funding (for new setups), it will automatically create and broadcast a fidelity transaction using the funding UTXOs:

  ```bash
  INFO coinswap::wallet::fidelity - Fidelity Transaction 4593a892809621b64418d6bf9590c6536a1fa27f7a136d176ad302fb8ec3ce23 seen in mempool, waiting for confirmation.
  ```
  
- **Fidelity Transaction Confirmation**: Once the transaction is confirmed:
  
  ```bash
  INFO coinswap::wallet::fidelity - Fidelity Transaction 4593a892809621b64418d6bf9590c6536a1fa27f7a136d176ad302fb8ec3ce23 confirmed at blockheight: 229349
  INFO coinswap::maker::server - [6102] Successfully created fidelity bond
  ```


- **Thread Spawning**: Several threads will be spawned to handle specific tasks:

  ```bash
  INFO coinswap::maker::server - [6102] Spawning contract-watcher thread
  INFO coinswap::maker::server - [6102] Spawning Client connection status checker thread
  INFO coinswap::maker::server - [6102] Spawning RPC server thread
  INFO coinswap::maker::rpc::server - [6102] RPC socket binding successful at 127.0.0.1:6103
  ```

- **Server Ready**: Finally, the `makerd` server is fully set up and ready to connect with other takers for coin swaps:

```bash
INFO coinswap::maker::server - [6102] Server Setup completed!! Use maker-cli to operate the server and the internal wallet.
```

The server will display information about swap liquidity and continue listening for requests:

```bash
INFO coinswap::maker::server - Swap Liquidity: 5001672 sats | Min: 10000 sats | Listening for requests.
INFO coinswap::maker::server - [6102] Bitcoin Network: regtest
INFO coinswap::maker::server - [6102] Spendable Wallet Balance: 0.05001672 BTC
```

---

For detailed instructions on how to use the maker-cli, please refer to the [maker-cli demo](./maker-cli.md). This guide will provide a comprehensive overview of the available commands and features for operating your maker server effectively.

---

# Setting up `bitcoind`

This tutorial will guide you through setting up `bitcoind` on a machine, running a Bitcoin `regtest` node, and performing basic operations like creating a wallet, generating blocks, and checking the balance.

## 1. Installing `bitcoind`

To start working with Bitcoin, it's necessary to install the Bitcoin Core software, which includes `bitcoind` (the Bitcoin daemon).

### Steps:

1. **Get Bitcoin Core:**
   - Visit the [official Bitcoin Core website](https://bitcoin.org/en/download) and download the appropriate version for the operating system.
   - Follow the instructions on the site to verify the downloaded files by checking the signatures to ensure authenticity.

2. **Verify Installation:**
   - After installation, run the following command to confirm that `bitcoind` is properly installed:
     ```bash
     bitcoind --version
     ```
   - This will output the version of `bitcoind` if it's installed correctly.

## 2. Setting up a `bitcoin.conf` File

While `bitcoind` can be run on various Bitcoin networks, this tutorial will focus on the `regtest` network, which is a local blockchain environment ideal for development and testing.

Before running `bitcoind`, it's important to configure the node with a `bitcoin.conf` file to set up the `regtest` network.

### Sample `bitcoin.conf` File for `regtest`

First, create a directory for Bitcoin data and configuration files if it doesn’t already exist:

```bash
mkdir -p ~/.bitcoin
```
Then, create a `bitcoin.conf` file in `~/.bitcoin/` and add the following lines:

```ini
regtest=1
server=1
fallbackfee=0.0001
rpcuser=admin
rpcpassword=password
rpcbind=0.0.0.0:18443
rpcallowip=0.0.0.0/0
txindex=1
```

### Explanation of Configurations:

- `regtest=1`: Runs the node in regtest mode.
- `server=1`: Enables `bitcoind` to run as a server and accept RPC (Remote Procedure Call) commands.
- `rpcuser` and `rpcpassword`: Set the username and password for `bitcoin-cli` RPC access. These values can be customized or left as provided.
- `rpcbind=0.0.0.0:18443`: Binds the RPC server to this IP and port.
- `rpcallowip=0.0.0.0/0`: Allows RPC connections from any IP address. Be cautious when using this in a non-development environment.
- `fallbackfee=0.0001`: Specifies a fallback transaction fee when none is provided.
- `txindex=1`: Enables a full transaction index for your node, which is useful for querying historical transactions.

After setting up the configuration file, your node will be ready to run in `regtest` mode.

## 3. Basic Operations

Once the `bitcoin.conf` file is configured, `bitcoind` can be started, and basic operations can be performed using `bitcoin-cli`.



### 3.1 Start the `bitcoind` daemon:

Run the following command to start the Bitcoin node:
```bash
bitcoind -daemon
```

- **Note**: we don't need to pass the flag to mention on the bitcoin mention on which we want to run the bitcoind as `bitcoin.conf` already mentions the bitcoin network.

Check the status to ensure it’s running:

```bash
bitcoin-cli getblockchaininfo
```

This command provides details about the blockchain's current state, including the number of blocks and synchronization status.

### 3.2 Create a Wallet

Create a wallet to perform wallet-related operations in the `regtest` environment:

```bash
bitcoin-cli createwallet "testwallet"
```

### 3.3 Get a New Bitcoin Address

Generate a new Bitcoin address to receive funds:

```bash
bitcoin-cli getnewaddress
```

### 3.4 Generate Some Blocks

Since this is regtest, so we can generate new blocks and receive Bitcoin as block rewards:

```bash
bitcoin-cli generatetoaddress 101 <address>
```

### 3.5 Check the wallet balance:
Check the balance in your wallet:

```bash
bitcoin-cli getbalance
```

This should display a balance corresponding to the block rewards from the generated blocks.

That's everything needed to get started with `bitcoind` on the `regtest` network. Now you're ready to explore Bitcoin transactions and experiment with coinswap CLI apps.


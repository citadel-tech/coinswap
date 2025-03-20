# Working with `bitcoind` in regtest

In this tutorial, we will guide you through setting up a Bitcoin Core `bitcoind` node in `regtest` mode. We'll cover basic operations like creating wallets, generating blocks, checking balances, and sending Bitcoin between two wallets.

---

### **1. Installing `bitcoind`**

To start working with Bitcoin, it's necessary for us to install the Bitcoin Core software, which includes the `bitcoind` daemon.

#### **Steps:**

1. **Get Bitcoin Core:**

   - We will visit the [official Bitcoin Core website](https://bitcoin.org/en/download) and download the appropriate version for our operating system.
   - After downloading, we’ll follow the instructions on the site to verify the downloaded files by checking the signatures to ensure authenticity.

2. **Verify Installation:**
   - After installation, we can run the following command to confirm that `bitcoind` is properly installed:
     ```bash
     bitcoind --version
     ```
   - This will output the version of `bitcoind` if it's installed correctly.

---

### **2. Setting up a `bitcoin.conf` File**

While `bitcoind` can be run on various Bitcoin networks, we will focus on the `regtest` network in this tutorial, which is a local blockchain environment ideal for development and testing. Before running `bitcoind`, we need to configure the node with a `bitcoin.conf` file to set up the `regtest` network.

#### **Sample `bitcoin.conf` File for `regtest`**

First, we’ll create a directory for Bitcoin data and configuration files if it doesn’t already exist:

```bash
mkdir -p ~/.bitcoin
```

Then, we’ll create a `bitcoin.conf` file in `~/.bitcoin/` and add the following lines:

```ini
regtest=1 # change this to change Bitcoin Network
server=1
fallbackfee=0.0001 # for regtest only
rpcuser=user
rpcpassword=password
rpcallowip=0.0.0.0/0
txindex=1
```

#### **Explanation of Configurations:**

- `regtest=1`: Runs the node in regtest mode.
- `server=1`: Enables `bitcoind` to run as a server and accept RPC (Remote Procedure Call) commands.
- `rpcuser` and `rpcpassword`: Set the username and password for `bitcoin-cli` RPC access. We can customize these values or leave them as provided.
- `rpcallowip=0.0.0.0/0`: Allows RPC connections from any IP address. We should be cautious when using this in a non-development environment.
- `txindex=1`: Enables a full transaction index for our node, which is useful for querying historical transactions.

After setting up the configuration file, our node will be ready to run in `regtest` mode.



> **NOTE**: `Regtest` is a toy network that allows for creating custom blocks and generating coins, making it ideal for easy integration testing of applications.  
> For actual swap markets, use `testnet4`. Switch to `testnet4` by setting `testnet4=1` in the configuration.
---

### **3. Basic Operations**

Once the `bitcoin.conf` file is configured, we can start `bitcoind` and perform basic operations using `bitcoin-cli`.

#### **3.1 Start the `bitcoind` daemon:**

We run the following command to start the Bitcoin node:

```bash
$ bitcoind 
```

- **Note**: We don’t need to specify the network explicitly since the `bitcoin.conf` file already defines the network.

To check the status of the node and confirm that it's running, we can use:

```bash
$ bitcoin-cli getblockchaininfo
```

This will output the current state of the blockchain, including the number of blocks and synchronization status, as shown:

```bash
{
  "chain": "regtest",
  "blocks": 0,
  "headers": 0,
  "bestblockhash": "0f9188f13cb7b2c71f2a335e3a4fc328bf5beb436012afca590b1a11466e2206",
  "difficulty": 4.656542373906925e-10,
  "time": 1296688602,
  "mediantime": 1296688602,
  "verificationprogress": 1,
  "initialblockdownload": true,
  "chainwork": "0000000000000000000000000000000000000000000000000000000000000002",
  "size_on_disk": 293,
  "pruned": false,
  "warnings": ""
}
```

#### **3.2 Create a Wallet for Alice**

We create a wallet called `alice` to perform wallet-related operations in the `regtest` environment:

```bash
$ bitcoin-cli createwallet "alice"
```

The response will confirm that the wallet `alice` has been created:

```json
{
  "name": "alice"
}
```

#### **3.3 Create a Wallet for Bob**

Similarly, we create another wallet called `bob`:

```bash
$ bitcoin-cli createwallet "bob"
```

The response will confirm that the wallet `bob` has been created:

```json
{
  "name": "bob"
}
```

#### **3.4 Get a New Bitcoin Address for Alice**

We generate a new Bitcoin address for `alice` to receive funds:

```bash
$ bitcoin-cli -rpcwallet=alice getnewaddress
```

This returns a new address for `alice`:

```bash
bcrt1qfvgecwpwtn77f7vv6wfc78zdcxseq4pjpyn9jv
```

#### **3.5 Generate Some Blocks for Alice**

Since we’re using `regtest`, we can generate new blocks to the generated address for `alice` and receive Bitcoin as block rewards:

```bash
$ bitcoin-cli -rpcwallet=alice generatetoaddress 101 <alice_address>
```

This will return a list of block hashes in hex format, corresponding to the 101 newly generated blocks as shown: 

```bash
[
  "00b968e6627d8f160369a06a3169719487cf246a5d43afa113c42a236305b7d3",
  "3ac6820a58627c9e69dcd349d9d152909b018a2afe923bed73dae0c7b7134104",
  "0d4b1870a18216450e5da68c7349e7cb26beb144c671524c23798728ca222cd8",
  "41c1c331c93e75f2048a285c925e15eca423b0a93f1f7354261adef8f29186c3",

  ... till 101 block hashes
]
```

#### **3.6 Check Alice's Wallet Balance**

We can now check the balance in `alice`'s wallet:

```bash
$ bitcoin-cli -rpcwallet=alice getbalance
```

This will show a balance corresponding to the block rewards for the generated blocks:

```bash
{
  "mine": {
    "trusted": 50.00000000,
    "untrusted_pending": 0.00000000,
    "immature": 0.00000000
  }
}
```

---

### **4. Sending Bitcoin from Alice to Bob**

Now, let’s send 1 BTC from `alice` to `bob` using the `sendtoaddress` RPC command.

#### **4.1 Get Bob's Bitcoin Address**

We generate a new Bitcoin address for `bob`:

```bash
$ bitcoin-cli -rpcwallet=bob getnewaddress
```

This returns a new address for `bob`:

```bash
bcrt1q2nys4aedf448ngt5gpw5gmun6gdjqgy04qj6cq
```

#### **4.2 Send 1 BTC from Alice to Bob**

Next, we send 1 BTC from `alice` to `bob` using the `sendtoaddress` command:

```bash
$ bitcoin-cli -rpcwallet=alice sendtoaddress <bob_address> 1
```

This will create and broadcast a signed transaction, returning the transaction hash:

```bash
0b0c6a25e16f987e5a68fe59c301e06615376837b11a3ed678b0f0bd8a69a18a
```

#### **4.3 Generate a Block to Confirm the Transaction**

We generate a block to confirm the transaction to `bob`:

```bash
$ bitcoin-cli -rpcwallet=bob generatetoaddress 1 <bob_address>
```

This will return the block hash in hex format:

```bash
"43e5d06abcf026e3faec392626545fb4880592682fd61645dc99d51e277bd761"
```

#### **4.4 Check the Final Balance of Each Wallet**

Finally, we check the balance of both wallets to confirm that the transaction was successful.

##### **For Bob's Wallet:**

```bash
$ bitcoin-cli -rpcwallet=bob getbalances
```

The response should show that `bob` has received the 1 BTC:

```json
{
  "mine": {
    "trusted": 1.00000000,
    "untrusted_pending": 0.00000000,
    "immature": 0.00000000
  }
}
```

##### **For Alice's Wallet:**

```bash
$ bitcoin-cli -rpcwallet=alice getbalances
```

The response will show that `alice`'s balance has been reduced by a little more than 1 BTC, where 1 BTC has been sent to `bob`, and the remaining amount goes toward mining fees for confirming the transaction:

```json
{
  "mine": {
    "trusted": 48.99998590,
    "untrusted_pending": 0.00000000,
    "immature": 0.00000000
  }
}
```

---

We’ve set up `bitcoind`, created two wallets (`alice` and `bob`), generated blocks, and sent Bitcoin between the wallets. This provides everything we need to start with `bitcoind` on `regtest`. Now, we’re ready to explore Bitcoin transactions and experiment with coinswap CLI apps.


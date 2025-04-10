## Working with `bitcoind` in Regtest Mode

This tutorial walks you through setting up a Bitcoin Core `bitcoind` node in `regtest` mode. We'll cover basic operations like creating wallets, generating blocks, checking balances, and sending Bitcoin between two wallets.

---

### 1. Installing `bitcoind`

To begin, install the Bitcoin Core software which includes the `bitcoind` daemon.

#### Steps:

1. **Download Bitcoin Core:**
   - Visit the [official Bitcoin Core website](https://bitcoin.org/en/download) and download the version appropriate for your OS.
   - Verify the download by checking the signatures to ensure authenticity.

2. **Verify Installation:**
   - Run the following command to confirm installation:
     ```bash
     bitcoind --version
     ```

---

### 2. Setting up `bitcoin.conf` for Regtest

Configure your node to run in `regtest` mode by creating a `bitcoin.conf` file.

#### Sample Configuration:
```bash
mkdir -p ~/.bitcoin
nano ~/.bitcoin/bitcoin.conf
```

Paste the following into `bitcoin.conf`:
```ini
regtest=1
server=1
fallbackfee=0.0001
rpcuser=user
rpcpassword=password
rpcallowip=0.0.0.0/0
txindex=1
```

> **Note**: For swap markets, consider using `testnet4` instead of `regtest` by setting `testnet4=1`.

---

### 3. Basic Operations

#### 3.1 Start `bitcoind`
```bash
bitcoind
```

Check status:
```bash
bitcoin-cli getblockchaininfo
```

#### 3.2 Create Wallets
```bash
bitcoin-cli createwallet "alice"
bitcoin-cli createwallet "bob"
```

#### 3.3 Get Address and Generate Blocks
```bash
bitcoin-cli -rpcwallet=alice getnewaddress
bitcoin-cli -rpcwallet=alice generatetoaddress 101 <alice_address>
```

#### 3.4 Check Balance
```bash
bitcoin-cli -rpcwallet=alice getbalance
```

---

### 4. Sending Bitcoin from Alice to Bob

#### 4.1 Get Bob's Address
```bash
bitcoin-cli -rpcwallet=bob getnewaddress
```

#### 4.2 Send BTC from Alice to Bob
```bash
bitcoin-cli -rpcwallet=alice sendtoaddress <bob_address> 1
```

#### 4.3 Confirm Transaction
```bash
bitcoin-cli -rpcwallet=bob generatetoaddress 1 <bob_address>
```

#### 4.4 Final Balances
```bash
bitcoin-cli -rpcwallet=bob getbalances
bitcoin-cli -rpcwallet=alice getbalances
```

---

With this, you've set up a fully functioning `bitcoind` regtest environment. You're now ready to explore Bitcoin transactions and integrate with CoinSwap CLI tools.

---
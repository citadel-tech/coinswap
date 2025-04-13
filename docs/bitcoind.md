# Working with `bitcoind` in Regtest Mode

In this tutorial, we will guide you through setting up a Bitcoin Core `bitcoind` node in `regtest` mode. We'll cover basic operations like creating wallets, generating blocks, checking balances, and sending Bitcoin between two wallets.

---

## 1. Install Bitcoin Core

### Steps:

1. **Download Bitcoin Core**

   - Visit the [official Bitcoin Core website](https://bitcoin.org/en/download) and download the appropriate version for your operating system.
   - Verify the downloaded files by checking their signatures to ensure authenticity and security.

2. **Verify Installation**

   After installation, confirm the installation with:

   ```bash
   bitcoind --version
   ```

   This should output the installed version of `bitcoind`.

---

## 2. Configure Bitcoin in Regtest Mode

Create a configuration file to enable the `regtest` network and setup RPC access.

### Sample `bitcoin.conf`

Create the directory (if not already created):

```bash
mkdir -p ~/.bitcoin
```

Then create or edit `~/.bitcoin/bitcoin.conf`:

```ini
regtest=1
server=1
fallbackfee=0.0001
rpcuser=user
rpcpassword=password
rpcallowip=127.0.0.1
txindex=1
```

> ⚠️ **Note**: `rpcallowip=127.0.0.1` restricts RPC access to localhost. Avoid `0.0.0.0/0` in production setups.

---

## 3. Start Bitcoin Node in Regtest Mode

Start the node:

```bash
bitcoind
```

Confirm it’s running:

```bash
bitcoin-cli getblockchaininfo
```

Expected output snippet:

```json
{
  "chain": "regtest",
  "blocks": 0,
  ...
}
```

---

## 4. Create Wallets

### Alice’s Wallet:

```bash
bitcoin-cli createwallet "alice"
```

### Bob’s Wallet:

```bash
bitcoin-cli createwallet "bob"
```

---

## 5. Generate Address for Alice

```bash
bitcoin-cli -rpcwallet=alice getnewaddress
```

Example result:

```
bcrt1qfvgecwpwtn77f7vv6wfc78zdcxseq4pjpyn9jv
```

---

## 6. Mine 101 Blocks to Alice’s Address

Replace `<alice_address>` with the address from above:

```bash
bitcoin-cli -rpcwallet=alice generatetoaddress 101 <alice_address>
```

---

## 7. Check Alice’s Balance

```bash
bitcoin-cli -rpcwallet=alice getbalance
```

Expected result:

```
50.00000000
```

> The first 50 BTC are spendable; the rest are immature rewards.

---

## 8. Send Bitcoin from Alice to Bob

### Step 1: Get Bob’s Address

```bash
bitcoin-cli -rpcwallet=bob getnewaddress
```

Example result:

```
bcrt1q2nys4aedf448ngt5gpw5gmun6gdjqgy04qj6cq
```

### Step 2: Send 1 BTC

```bash
bitcoin-cli -rpcwallet=alice sendtoaddress <bob_address> 1
```

Returns:

```
<transaction_id>
```

### Step 3: Confirm the Transaction

```bash
bitcoin-cli -rpcwallet=bob generatetoaddress 1 <bob_address>
```

Returns:

```
<block_hash>
```

---

## 9. Verify Final Balances

### Bob:

```bash
bitcoin-cli -rpcwallet=bob getbalance
```

Expected result:

```
1.00000000
```

### Alice:

```bash
bitcoin-cli -rpcwallet=alice getbalance
```

Example:

```
48.99998590
```

---

##  Summary

- You installed and configured `bitcoind` in `regtest` mode.
- You created two wallets (`alice` and `bob`).
- You mined blocks and earned BTC.
- You performed a transaction and confirmed it.

You are now ready to test Bitcoin-based applications locally. Use this setup as the foundation for testing CLI tools like Coinswap or writing custom Bitcoin scripts.

>  For realistic testing in a live-like environment, consider using `testnet4`. To switch, replace `regtest=1` with `testnet=1` in `bitcoin.conf`.
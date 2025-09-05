# Coinswap Live Demo Prerequisite and Setup

This guide will help you prepare your system for participating in the Coinswap Live Demo. Follow these steps carefully to ensure a smooth experience during the demonstration.

## System Prerequisites

### Required Software

1. **Rust and Cargo**
   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   ```
   Verify installation:
   ```bash
   rustc --version
   cargo --version
   ```

2. **Bitcoin Core**
   ```bash
   # Download Bitcoin Core (latest stable version)
   wget https://bitcoincore.org/bin/bitcoin-core-28.1/bitcoin-28.1-x86_64-linux-gnu.tar.gz
   
   # Download and verify signatures
   wget https://bitcoincore.org/bin/bitcoin-core-28.1/SHA256SUMS
   wget https://bitcoincore.org/bin/bitcoin-core-28.1/SHA256SUMS.asc
   
   # Verify download
   sha256sum --check SHA256SUMS --ignore-missing
   
   # Extract binaries
   tar xzf bitcoin-28.1-x86_64-linux-gnu.tar.gz
   
   # Install to system
   sudo install -m 0755 -o root -g root -t /usr/local/bin bitcoin-28.1/bin/*
   ```
   
   Verify installation:
   ```bash
   bitcoind --version
   ```

3. **Build Dependencies**
   ```bash
   sudo apt-get update
   sudo apt install build-essential automake libtool
   ```

4. **Setup Tor**
   
   Coinswap requires Tor exclusively for all communication. You will need to have Tor running locally to run the apps.
   For comprehensive Tor setup instructions, follow the [Tor Documentation](tor.md). 
   
   Use the below sample `torrc` config for quick setup:
   
   ```ini
   ControlPort 9051
   CookieAuthentication 0
   SOCKSPort 9050
   ```

## Bitcoin Core Setup

### 1. Create Configuration File
```bash
mkdir -p ~/.bitcoin
```

Add to `~/.bitcoin/bitcoin.conf`:
```ini
signet=1

[signet]
server=1
txindex=1
rpcuser=user
rpcpassword=password
fallbackfee=0.00001000
blockfilterindex=1
addnode=172.81.178.3:38333
signetchallenge=0014c9e9f8875a25c3cc6d99ad3e5fd54254d00fed44
```

> **Note**: Change `signet=1` to `testnet4=1` if you want to run the apps on testnet4, or other networks.

This configuration connects your node to our custom signet. The custom signet is designed specifically for coinswap testing and has a block interval of 2 minutes.

**Custom Signet Resources** (accessible via Tor browser only):
- [Custom Signet Explorer](http://4imwp7kgajusoslqa7lnjq7phpt3gocm55tlekg5fp5xqy4ipoy5f6ad.onion/)   
- [Custom Signet Faucet](http://xjw3jlepdy35ydwpjuptdbu3y74gyeagcjbuyq7xals2njjxrze6kxid.onion/)

### 2. Start Bitcoin Core
```bash
bitcoind
```

Wait for the Initial Block Download to complete. Follow the `bitcoind` logs for IBD progress.

Verify it's running:
```bash
bitcoin-cli getblockchaininfo
```

## Compile The Apps

### Clone and Build
```bash
git clone https://github.com/citadel-tech/coinswap.git
cd coinswap
cargo build --release
```

After compilation, you will get the binaries in the `` folder.

### Install Binaries (Optional)
Install the necessary binaries on your system:
```bash
sudo install ./target/release/taker /usr/local/bin/
sudo install ./target/release/makerd /usr/local/bin/  
sudo install ./target/release/maker-cli /usr/local/bin/  
```

> **Note**: For this tutorial, we'll use the binaries directly from `./target/release/` or `./target/debug/` for development builds.

### Key Points About Command Arguments

All coinswap applications connect to Bitcoin Core via RPC. Understanding these parameters is essential:

- The `-r` or `--ADDRESS:PORT` option specifies the Bitcoin Core RPC address and port. By default, this is set to **`127.0.0.1:38332`**.

- The `-a` or `--USER:PASSWORD` option specifies the Bitcoin Core RPC authentication. By default, this is set to **`user:password`**.

- #### If you're using the **default configuration**:
  - You don't need to include these arguments when using default settings.

- #### If you're using a **custom configuration**:
  - Pass your custom values using the `-r` and `-a` options.

## For this tutorial, we'll use the default configuration. All commands will work without additional parameters if your `bitcoin.conf` matches the setup above.

---

## Running the Swap Server (Maker)

The swap server runs using two apps: `makerd` and `maker-cli`. The `makerd` app runs a background server, and `maker-cli` is used to operate the server using RPC commands.

### 1. Check Available Commands
```bash
$ makerd --help
```

This will display detailed information about `makerd` options and usage.

### 2. Start the Maker Server
```bash
$ makerd
```

This will spawn the maker server and you will start seeing logs. The server is operated with the `maker-cli` app.

### 3. Server Setup Process

To successfully set up the swap server, it needs:
- A fidelity bond (minimum 50,000 sats)
- Additional balance for swap liquidity
So a total initial balance of 75000 sats would be sufficient.

**What happens during startup:**

- **Wallet Creation/Loading**: If no wallet exists, `makerd` will create a new one and display the mnemonic for backup.

- **Fidelity Bond Check**: The server checks for existing fidelity bonds or creates a new one.

- **Funding Required**: If the wallet is empty, the server will log the funding address and required amount.

Example log output:
```bash
INFO coinswap::maker::server - No active Fidelity Bonds found. Creating one.
INFO coinswap::maker::server - Fund the wallet with at least 0.00051000 BTC at address: bcrt1q...
```

### 4. Fund the Wallet

Use the [Custom Signet Faucet](http://xjw3jlepdy35ydwpjuptdbu3y74gyeagcjbuyq7xals2njjxrze6kxid.onion/) (open in Tor browser) to send coins to the address shown in the logs.

Suggested amount: **0.01 BTC** (provides fidelity bond + swap liquidity)

### 5. Monitor Server Progress

Once funded, the server will automatically:
1. Create and broadcast the fidelity bond transaction
2. Wait for confirmation (approximately 2-4 minutes on custom signet)
3. Start listening for incoming swap requests

Final success log:
```bash
INFO coinswap::maker::server - [6102] Server Setup completed!! Use maker-cli to operate the server and the internal wallet.
```

### 6. Operate the Server with maker-cli

Open a new terminal and try these operations:

#### Check Server Status
```bash
$ maker-cli send-ping
```

#### View Balances
```bash
$ maker-cli get-balances
```

#### List UTXOs
```bash
$ maker-cli list-utxo
```

#### View Fidelity Bond Details
```bash
$ maker-cli show-fidelity
```

#### Get Server's Tor Address
```bash
$ maker-cli show-tor-address
```

All relevant files and wallets used by the server are located in the `~/.coinswap/maker/` data directory. **Important**: Back up the wallet file `~/.coinswap/maker/wallets/maker-wallet` to avoid loss of funds.

---

## Run The Swap Client (Taker)

The swap client is operated with the `taker` app.

### 1. Check Available Commands
```bash
$ taker --help
```

### 2. Get a Receiving Address
```bash
$ taker get-new-address
```

### 3. Fund the Taker Wallet

Use the [Custom Signet Faucet](http://xjw3jlepdy35ydwpjuptdbu3y74gyeagcjbuyq7xals2njjxrze6kxid.onion/) (open in Tor browser) to send some funds to the address above.

Suggested amount: **0.001 BTC** (sufficient for testing swaps)

### 4. Check Wallet Balance
```bash
$ taker get-balances
```

**Output:**
```json
{
  "contract": 0,
  "regular": 100000,
  "spendable": 100000,
  "swap": 0
}
```

### 5. Fetch Market Offers
```bash
$ taker fetch-offers
```

This command fetches all current market offers from available makers. You should see offer details including fees, minimum amounts, and maker addresses.

### 6. Perform a Coinswap
```bash
$ taker coinswap
```

This initiates the coinswap process with default parameters. The process will:
1. Select suitable makers from the offer book
2. Negotiate swap terms
3. Execute the multi-party coinswap protocol
4. Complete the swap transactions

Monitor the logs for swap progress. The process typically takes several minutes to complete.

### 7. Monitor Swap Progress

You can monitor detailed swap progress in the debug log:
```bash
tail -f ~/.coinswap/taker/debug.log
```

---

## Basic Troubleshooting

### Bitcoin Core Issues
- **Verify bitcoind is running**: 
  ```bash
  bitcoin-cli getblockchaininfo
  ```
- **Check RPC authentication**: Ensure `rpcuser` and `rpcpassword` in `bitcoin.conf` match what the apps expect
- **Verify correct network**: Confirm you're on the intended network (signet/testnet4)
- **Check IBD status**: Ensure Initial Block Download is complete

### Maker Server Issues
- **Check debug logs**: 
  ```bash
  tail -f ~/.coinswap/maker/debug.log
  ```
- **Verify fidelity bond creation**: Use `maker-cli show-fidelity` to check bond status
- **Ensure sufficient funds**: Check balances with `maker-cli get-balances`
- **Check Tor connection**: Verify Tor is running and properly configured

### Taker Client Issues
- **Verify wallet funding**: Use `taker get-balances` to check funds
- **Check network connectivity**: Ensure connection to makers via Tor
- **Monitor debug logs**: 
  ```bash
  tail -f ~/.coinswap/taker/debug.log
  ```
- **Offer book issues**: Try `taker fetch-offers` to verify maker connectivity

### Common Solutions
- **Restart Tor**: 
  ```bash
  sudo systemctl restart tor
  ```
- **Check firewall settings**: Ensure required ports are accessible
- **Verify Bitcoin Core sync**: Ensure node is fully synchronized
- **Check disk space**: Ensure sufficient space for blockchain data

### Getting Help

If you're still stuck and couldn't get the apps running:
- Join our [Discord](https://discord.gg/gs5R6pmAbR) and ask for help from developers directly
- Check the [GitHub Issues](https://github.com/citadel-tech/coinswap/issues) for known problems
- Review the detailed component guides listed below

---

## Additional Resources

### Documentation
- [Bitcoin Core Documentation](https://bitcoin.org/en/developer-reference)
- [Coinswap Protocol Specification](https://github.com/citadel-tech/Coinswap-Protocol-Specification)
- [Project Repository](https://github.com/citadel-tech/coinswap)

### Component Guides
For more detailed information about specific components:
- [Bitcoin Core Setup Guide](./bitcoind.md)
- [Maker Server Guide](./makerd.md)
- [Maker CLI Reference](./maker-cli.md)
- [Taker Client Guide](./taker.md)
- [Tor Setup & Configuration](./tor.md)

### Networks
- **Custom Signet**: Recommended for testing, 2-minute blocks
- **Testnet4**: Alternative testing network with longer block times
- **Regtest**: Local development only (not connected to other nodes)

### Security Notes
- **Backup wallet files**: Always backup mnemonic phrases and wallet files
- **Secure your server**: Use proper authentication for production deployments
- **Monitor logs**: Keep an eye on debug logs for any suspicious activity
- **Test thoroughly**: Start with small amounts when testing
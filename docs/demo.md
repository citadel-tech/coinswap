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
   # Download Bitcoin Core 28.1
   wget https://bitcoincore.org/bin/bitcoin-core-28.1/bitcoin-28.1-x86_64-linux-gnu.tar.gz
   
   # Download and verify signatures
   wget https://bitcoincore.org/bin/bitcoin-core-28.1//SHA256SUMS
   wget https://bitcoincore.org/bin/bitcoin-core-28.1//SHA256SUMS.asc
   
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
   
   Coinswap requires Tor exclusively for all communication. You will need to have Tor running in your local to run the apps.
   For Tor setup instructions follow the [Tor Doc](tor.md). 
   
   Use the below sample `torrc` config for quick setup.
   
   ```shell
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
```bash
regtest=1

[regtest]
server=1
txindex=1
rpcuser=user
rpcpassword=password
fallbackfee=0.00001000
blockfilterindex=1
addnode=172.81.178.3:18444
```

> **NOTE**: Change `regtest=1` to `testnet4=1` if you want to run the apps on testnet, or other networks.

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
```bash
git clone https://github.com/citadel-tech/coinswap.git
cd coinswap
cargo build --release
```

After compilation you will get the binaries in the `./target/release` folder. 

Install the necessary binaries in your system:
```bash
sudo install ./target/release/taker /usr/local/bin/
sudo install ./target/release/makerd /usr/local/bin/  
sudo install ./target/release/maker-cli /usr/local/bin/  
```

## Running the Swap Server

The swap server is run using two apps `makerd` and `maker-cli`. The `makerd` app runs a server, and `maker-cli` is used to operate the server using RPC commands.

Check the available `makerd` commands with
```bash
makerd --help
```

Start the `makerd` daemon with all default parameters:
```bash
makerd
```

This will spawn the maker server and you will start seeing the logs. The server is operated with the `maker-cli` app. Follow the log, and it will show you the next instructions.

To successfully set up the swap server, it needs to have a fidelity bond and enough balance (minimum 20,000 sats) to start providing swap services.

In the log you will see the server is asking for some BTC at a given address. Fund that address with the given minimum amount or more. We recommend using the [mempool.space Testnet4 faucet](https://mempool.space/testnet4/faucet), but you can use any other faucet of your choice.

Once the funds are sent, the server will automatically create a fidelity bond transaction, wait for its confirmation, and when confirmed, send its offers and details to the DNS server and start listening for incoming swap requests.

At this stage you can start using the `maker-cli` app to query the server and get all relevant details.

On a new terminal, try out a few operations like:
```bash
maker-cli --help
maker-cli get-balances
maker-cli list-utxo
```

If everything goes all right you will be able to see balances and utxos in the `maker-cli` outputs.

All relevant files and wallets used by the server will be located in `~/.coinswap/maker/` data directory. It's recommended to take a backup of the wallet file `~/.coinswap/maker/wallets/maker-wallet`, to avoid loss of funds.


## Run The Swap Client

The swap client is run with the `taker` app. 

From a new terminal, go to the project root directory and perform basic client operations:

### Get Some Money
```bash
taker get-new-address
```

Use a testnet4 faucet to send some funds at the above address. Then check the client wallet balance with
```bash
taker get-balances
```

### Fetch Market Offers
Fetch all the current existing market offers with 
```bash
taker fetch-offers
```

### Perform a Coinswap
Attempt a coinswap process with
```bash
taker coinswap
```

If all goes well, you will see the coinswap process starting in the logs.


## Basic Troubleshooting

### Bitcoin Core Issues
- Verify bitcoind is running: `bitcoin-cli getblockchaininfo`
- Check rpcuser/rpcpassword attempted by the apps are matching with the bitcoin.conf file values
- Ensure correct network (testnet4)

### Maker Server Issues
- Check debug.log for errors
- Verify fidelity bond creation
- Ensure sufficient funds for operations
- Check TOR connection status

### Taker Client Issues
- Verify wallet funding
- Check network connectivity
- Monitor debug.log for detailed errors

### Asking for further help
If you are still stuck and couldn't get the apps running, drop in our [Discord](https://discord.gg/gs5R6pmAbR) and ask help from the developers directly.

## Additional Resources

- [Bitcoin Core Documentation](https://bitcoin.org/en/developer-reference)
- [Coinswap Protocol Specification](https://github.com/citadel-tech/Coinswap-Protocol-Specification)
- [Project Repository](https://github.com/citadel-tech/coinswap)

For more detailed information about specific components:
- [Bitcoin Core Setup](./bitcoind.md)
- [Maker Server Guide](./makerd.md)
- [Maker CLI Reference](./maker-cli.md)
- [Taker Client Guide](./taker.md)
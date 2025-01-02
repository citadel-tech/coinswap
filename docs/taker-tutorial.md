# Taker Tutorial

The taker is the party that initiates the coinswap. It queries the directory server for a list of makers, requests offers from them and selects suitable makers for the swap. It then conducts the swap with the selected makers.

In this tutorial, we will guide you through the process of setting up and running the taker, and conducting a coinswap.

## Setup

### Bitcoin Core

In order to run the taker, you need to have Bitcoin Core installed and the `bitcoind` service running. You can download Bitcoin Core [here](https://bitcoin.org/en/bitcoin-core/). Now, before running the `bitcoind` service, you need to create a configuration file for it. Create a file named `bitcoin.conf` in the Bitcoin Core data directory (located at `$HOME/.bitcoin/` on Linux). Add the following lines to the file:

```
signet=1
server=1
txindex=1
rpcuser=user
rpcpassword=pass
```

This will make Bitcoin Core run on the Signet network, enable the RPC server, and set the RPC username and password to `user` and `pass` respectively. This is important for the taker to be able to interact with the Bitcoin Core service. The Signet network is a local testing network that allows you to mine blocks instantly and generate coins for testing purposes.

Save the file and start the `bitcoind` service by running the following command:

```
bitcoind -signet
```

This will start the Bitcoin Core service on the Signet network in the background.

## Taker CLI

The taker CLI is an application that allows you to perform coinswaps as a taker.

### Installation

[TODO]

### Usage

Run the `taker` command to see the list of available commands and options.

```sh
$ taker

coinswap 0.1.0
Developers at Citadel-Tech
A simple command line app to operate as coinswap client

USAGE:
    taker [OPTIONS] <SUBCOMMAND>

OPTIONS:
    -a, --USER:PASSWORD <USER:PASSWORD>
            Bitcoin Core RPC authentication string. Ex: username:password [default: user:password]

    -d, --data-directory <DATA_DIRECTORY>
            Optional data directory. Default value : "~/.coinswap/taker"

    -h, --help
            Print help information

    -r, --ADDRESS:PORT <ADDRESS:PORT>
            Bitcoin Core RPC address:port value [default: 127.0.0.1:18443]

    -v, --verbosity <VERBOSITY>
            Sets the verbosity level of debug.log file [default: info] [possible values: off, error,
            warn, info, debug, trace]

    -V, --version
            Print version information

    -w, --WALLET <WALLET>
            Sets the taker wallet's name. If the wallet file already exists, it will load that
            wallet. Default: taker-wallet

SUBCOMMANDS:
    do-coinswap             Initiate the coinswap process
    fetch-offers            Update the offerbook with current market offers and display them
    get-balance             Get the total spendable wallet balance (sats)
    get-balance-contract    Get the total amount stuck in HTLC contracts (sats)
    get-balance-swap        Get the total balance received from swaps (sats)
    get-new-address         Returns a new address
    help                    Print this message or the help of the given subcommand(s)
    list-utxo               Lists all currently spendable utxos
    list-utxo-contract      Lists all HTLC utxos (if any)
    list-utxo-swap          Lists all utxos received in incoming swaps
    send-to-address         Send to an external wallet address
```

In order to do a coinswap, we first need to get some coins in our wallet. Let's generate a new address and send some coins to it.

```sh
$ taker -r 127.0.0.1:38332 -a user:pass get-new-address

bcrt1qyywgd4we5y7u05lnrgs8runc3j7sspwqhekrdd
```

Now we can use a Signet faucet to send some coins to this address. You can find a Signet faucet [here](https://signetfaucet.com/).

Once you have some coins in your wallet, you can check your balance by running the following command:

```sh
$ taker -r 127.0.0.1:38332 -a user:pass get-balance

10000000 SAT
```

Now we are ready to initate a coinswap. We are first going to sync the offer book to get a list of available makers.

```sh
$ taker -r 127.0.0.1:38332 -a user:pass fetch-offers
```

This will fetch the list of available makers from the directory server. Now we can initiate a coinswap with the makers.

```sh
$ taker -r 127.0.0.1:38332 -a user:pass do-coinswap
```

This will initiate a coinswap with the default parameters.

## Data, Config and Wallets

The taker stores all its data in a data directory. By default, the data directory is located at `$HOME/.coinswap/taker`. You can change the data directory by passing the `--data-directory` option to the `taker` command.

The data directory contains the following files:

1. `config.toml` - The configuration file for the taker.
2. `debug.log` - The log file for the taker.
3. `wallets` directory - Contains the wallet files for the taker.

### Configuration

The configuration is stored in the `config.toml` file. You can edit this file to change the configuration of the taker. The configuration file contains the following fields:

1. `port` - The port via which the Taker listens and serves requests.
2. `socks_port` - The port via which the Taker listens and serves requests for the Socks5 proxy.
3. `rpc_port` - The port which serves the RPC server.
4. `directory_server_address` - The address of the directory server.
5. `connection_type` - The connection type to use for the directory server. Possible values are `CLEARNET` and `TOR`.

### Wallets

The taker uses wallet files to store the wallet data. The wallet files are stored in the `wallets` directory. These wallet files should be safely backed up as they contain the private keys to the wallet.

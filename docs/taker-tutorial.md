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
developers at citadel-tech
taker-cli is a command line app to use taker client API's

USAGE:
    taker [OPTIONS] [ARGS] <SUBCOMMAND>

ARGS:
    <maker_count>          Sets the maker count to initiate coinswap with [default: 2]
    <send_amount>          Sets the send amount [default: 500000]
    <tx_count>             Sets the transaction count [default: 3]
    <required_confirms>    Sets the required on-chain confirmations [default: 1000]

OPTIONS:
    -a, --USER:PASSWORD <USER:PASSWORD>
            Sets the rpc basic authentication [default: user:password]

    -b, --bitcoin-network <BITCOIN_NETWORK>
            Sets the full node network, this should match with the network of the running node
            [default: regtest] [possible values: regtest, signet, mainnet]

    -c, --connection-type <CONNECTION_TYPE>
            Optional Connection Network Type [default: clearnet] [possible values: tor, clearnet]

    -d, --data-directory <DATA_DIRECTORY>
            Optional DNS data directory. Default value : "~/.coinswap/taker"

    -h, --help
            Print help information

    -r, --ADDRESS:PORT <ADDRESS:PORT>
            Sets the full node address for rpc connection [default: 127.0.0.1:18443]

    -v, --verbosity <VERBOSITY>
            Sets the verbosity level of logs. Default: Determined by the command passed [possible
            values: off, error, warn, info, debug, trace]

    -V, --version
            Print version information

    -w, --WALLET <WALLET>
            Sets the taker wallet's name. If the wallet file already exists at data-directory, it
            will load that wallet

SUBCOMMANDS:
    contract-balance    Returns the total live contract balance
    contract-utxo       Returns a list of live contract utxos
    do-coinswap         Initiate the coinswap process
    get-new-address     Returns a new address
    help                Print this message or the help of the given subcommand(s)
    seed-balance        Returns the total seed balance
    seed-utxo           Returns a list of seed utxos
    send-to-address     Send to an external wallet address
    swap-balance        Returns the total swap coin balance
    swap-utxo           Returns a list of swap coin utxos
    sync-offer-book     Sync the offer book
    total-balance       Returns the total balance of taker wallet
```

In order to do a coinswap, we first need to get some coins in our wallet. Let's generate a new address and send some coins to it.

```sh
$ taker -r 127.0.0.1:38332 -a user:pass get-new-address
bcrt1qyywgd4we5y7u05lnrgs8runc3j7sspwqhekrdd
```

Now we can use a Signet faucet to send some coins to this address. You can find a Signet faucet [here](https://signetfaucet.com/).

Once you have some coins in your wallet, you can check your balance by running the following command:

```sh
$ taker -r 127.0.0.1:38332 -a user:pass seed-balance
10000000 SAT
```

Now we are ready to initate a coinswap. We are first going to sync the offer book to get a list of available makers.

```sh
$ taker -r 127.0.0.1:38332 -a user:pass sync-offer-book
```

This will fetch the list of available makers from the directory server. Now we can initiate a coinswap with the makers.

```sh
$ taker -r 127.0.0.1:38332 -a user:pass do-coinswap
```

This will initiate a coinswap with the default parameters.
# Taker Tutorial

The **Taker** is the party that initiates the coinswap. It queries the directory server for a list of makers, requests offers from them and selects suitable makers for the swap. It then conducts the swap with the selected makers.

In this tutorial, we will guide you through the process of setting up and running the taker, and conducting a coinswap.

## Setup


## Taker CLI

The taker CLI is an application that allows you to perform coinswaps as a taker.

> **Warning:**  
> Taker wallet files contain private keys required to spend your funds. Ensure these wallet files are backed up securely, and take appropriate measures to protect your private keys.

### Start Bitcoin Core (Pre-requisite)

`Taker` requires a **Bitcoin Core** RPC connection running on a **custom signet** for its operation(check demo doc](./demo.md)). To get started, you need to start `bitcoind`:

> **Important:**  
> All apps are designed to run on our **custom signet** for testing purposes. The DNS server that Taker connects to will also be on signet. While you can run these apps on other networks, there won't be any DNS available, so Taker won't be able to connect to the DNS server for getting maker's offers and can't do coinswap with makers. Alternatively, you can run your own DNS server on the network of your choice.

To start `bitcoind`:

```bash
$ bitcoind
```

**Note:** If you don’t have `bitcoind` installed or need help setting it up, refer to the [bitcoind demo documentation](./bitcoind.md).


### Usage

Run the `taker` command to see the list of available commands and options.

```sh
$ ./taker --help
coinswap 0.1.0
Developers at Citadel-Tech
A simple command line app to operate as coinswap client.

The app works as regular Bitcoin wallet with added capability to perform coinswaps. The app requires
a running Bitcoin Core node with RPC access.
Use this faucet: http://xjw3jlepdy35ydwpjuptdbu3y74gyeagcjbuyq7xals2njjxrze6kxid.onion/(open in Tor browser) to get some signet coins.

For more detailed usage information, please refer:
https://github.com/citadel-tech/coinswap/blob/master/docs/app%20demos/taker.md

This is early beta, and there are known and unknown bugs. Please report issues at:
https://github.com/citadel-tech/coinswap/issues

USAGE:
    taker [OPTIONS] <SUBCOMMAND>

OPTIONS:
    -a, --USER:PASSWORD <USER:PASSWORD>
            Bitcoin Core RPC authentication string. Ex: username:password
            
            [default: user:password]

    -d, --data-directory <DATA_DIRECTORY>
            Optional data directory. Default value : "~/.coinswap/taker"

    -h, --help
            Print help information

    -r, --ADDRESS:PORT <ADDRESS:PORT>
            Bitcoin Core RPC address:port value
            
            [default: 127.0.0.1:38332]

    -t, --tor-auth <TOR_AUTH>
            [default: ]

    -v, --verbosity <VERBOSITY>
            Sets the verbosity level of debug.log file
            
            [default: info]
            [possible values: off, error, warn, info, debug, trace]

    -V, --version
            Print version information

    -w, --WALLET <WALLET>
            Sets the taker wallet's name. If the wallet file already exists, it will load that
            wallet. Default: taker-wallet

SUBCOMMANDS:
    coinswap
            Initiate the coinswap process
    fetch-offers
            Update the offerbook with current market offers and display them
    get-balances
            Get total wallet balances of different categories. regular: All single signature regular
            wallet coins (seed balance). swap: All 2of2 multisig coins received in swaps. contract:
            All live contract transaction balance locked in timelocks. If you see value in this
            field, you have unfinished or malfinished swaps. You can claim them back with recover
            command. spendable: Spendable amount in wallet (regular + swap balance)
    get-new-address
            Returns a new address
    help
            Print this message or the help of the given subcommand(s)
    list-utxo
            Lists all utxos we know about along with their spend info. This is useful for debugging
    list-utxo-contract
            Lists all utxos that we need to claim via timelock. If you see entries in this list, do
            a `taker recover` to claim them
    list-utxo-regular
            List all single signature wallet UTXOs. These are all non-swap regular wallet utxos
    list-utxo-swap
            Lists all utxos received in incoming swaps
    recover
            Recover from all failed swaps
    send-to-address
            Send to an external wallet address
```

In order to do a coinswap, we first need to get some coins in our wallet. Let's generate a new address and send some coins to it.

```sh
$ taker -r 127.0.0.1:38332 -a user:pass get-new-address

bcrt1qyywgd4we5y7u05lnrgs8runc3j7sspwqhekrdd
```

Now we can use the signet faucet to send some coins to this address. Use [this faucet](http://xjw3jlepdy35ydwpjuptdbu3y74gyeagcjbuyq7xals2njjxrze6kxid.onion/)(open in Tor browser) to get some signet coins.

Once you have some coins in your wallet, you can check your balance by running the following command:

```sh
$ taker -r 127.0.0.1:38332 -a user:pass get-balances

{
    "regular": 10000000,
    "swap": 0,
    "contract": 0,
    "spendable": 10000000
}
```

The balance categories are explained as follows:
- `regular`: All single signature regular wallet coins (seed balance)
- `swap`: All 2of2 multisig coins received in swaps
- `contract`: All live contract transaction balance locked in timelocks (if you see value here, you have unfinished or failed swaps)
- `spendable`: Total spendable amount in wallet (regular + swap balance)

Now we are ready to initiate a coinswap. We are first going to sync the offer book to get a list of available makers.

```sh
$ taker -r 127.0.0.1:38332 -a user:pass fetch-offers
```

This will fetch the list of available makers from the directory server. Now we can initiate a coinswap with the makers.

```sh
$ taker -r 127.0.0.1:38332 -a user:pass coinswap
```

This will initiate a coinswap with the default parameters. This will take some time. You can check swap progress at the log file in data directory. In a new terminal do `tail -f <datadir>/debug.log`.

### Recovering Failed Swaps

If a swap fails for any reason, the funds might be locked in a timelock contract. To check if you have any such locked funds, run:

```sh
$ ./taker -r 127.0.0.1:38332 -a user:pass list-utxo-contract
```

If you see any UTXOs in the output, you can recover them using the `recover` command:

```sh
$ ./taker -r 127.0.0.1:38332 -a user:pass recover
```

This will attempt to recover all funds from failed swaps.

## Data, Config and Wallets

The taker stores all its data in a data directory. By default, the data directory is located at `$HOME/.coinswap/taker`. You can change the data directory by passing the `--data-directory` option to the `taker` command.

The data directory contains the following files:

1. `config.toml` - The configuration file for the taker.
2. `debug.log` - The log file for the taker.
3. `wallets` directory - Contains the wallet files for the taker.


**Default Taker Configuration (`~/.coinswap/taker/config.toml`):**

```toml
control_port = 9051
socks_port = 9050
tor_auth_password = ""
directory_server_address = "ri3t5m2na2eestaigqtxm3f4u7njy65aunxeh7aftgid3bdeo3bz65qd.onion:8080"
connection_type = "TOR"
```
 
- `control_port`: The Tor Control Port. Check the [tor doc](tor.md) for more details.
- `socks_port`: The Tor Socks Port. Check the [tor doc](tor.md) for more details.
- `tor_auth_password`: Optional password for Tor control authentication; empty by default.
- `directory_server_address`: Address of the Directory Server (an onion address in production) for discovering Maker nodes.
- `connection_type`: The connection type to use for the directory server. Possible values are `CLEARNET` and `TOR`.

### Wallets

The taker uses wallet files to store the wallet data. The wallet files are stored in the `wallets` directory. These wallet files should be safely backed up as they contain the private keys to the wallet.

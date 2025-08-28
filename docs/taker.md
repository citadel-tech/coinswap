# Taker Tutorial

The **Taker** is the party that initiates the coinswap. It discovers makers, requests offers from them and selects suitable makers for the swap.

In this tutorial, we will guide you through the process of setting up and running the taker, and conducting a coinswap.

## Setup

## Taker CLI

The taker CLI is an application that allows you to perform coinswaps as a taker.

> **Warning:**  
> Taker wallet files contain private keys required to spend your funds. Ensure these wallet files are backed up securely, and take appropriate measures to protect your private keys.

### Start Bitcoin Core (Pre-requisite)

`Taker` requires a **Bitcoin Core** RPC connection running on a **custom signet** for its operation(check [demo doc](./demo.md)). To get started, you need to start `bitcoind`:

To start `bitcoind`:

```bash
$ bitcoind
```

**Note:** If you don't have `bitcoind` installed or need help setting it up, refer to the [bitcoind demo documentation](./bitcoind.md).

### Usage

Run the `taker` command to see the list of available commands and options:

```bash
$ ./taker --help
```

This will display a detailed guide about the app and its capabilities.

#### **Output:**

```bash
coinswap 0.1.2
Developers at Citadel-Tech
A simple command line app to operate as coinswap client.

The app works as a regular Bitcoin wallet with the added capability to perform coinswaps. The app
requires a running Bitcoin Core node with RPC access. It currently only runs on Testnet4. Suggested
faucet for getting Testnet4 coins: https://mempool.space/testnet4/faucet

For more detailed usage information, please refer:
https://github.com/citadel-tech/coinswap/blob/master/docs/taker.md

This is early beta, and there are known and unknown bugs. Please report issues at:
https://github.com/citadel-tech/coinswap/issues

USAGE:
    taker [OPTIONS] <SUBCOMMAND>

OPTIONS:
    -a, --USER:PASSWORD <USER:PASSWORD>
            Bitcoin Core RPC authentication string. Ex: username:password
            
            [default: user:password]

    -d, --data-directory <DATA_DIRECTORY>
            Optional data directory. Default value: "~/.coinswap/taker"

    -h, --help
            Print help information

    -r, --ADDRESS:PORT <ADDRESS:PORT>
            Bitcoin Core RPC address:port value
            
            [default: 127.0.0.1:48332]

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
            field, you have unfinished or malfinished swaps. You can claim them back with the
            recover command. spendable: Spendable amount in wallet (regular + swap balance)
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
            Lists all single signature wallet Utxos. These are all non-swap regular wallet utxos
    list-utxo-swap
            Lists all utxos received in incoming swaps
    recover
            Recover from all failed swaps
    send-to-address
            Send to an external wallet address
```

### Key Points About Command Arguments

- The `-r` or `--ADDRESS:PORT` option specifies the Bitcoin Core RPC address and port. By default, this is set to `127.0.0.1:48332`.

- The `-a` or `--USER:PASSWORD` option specifies the Bitcoin Core RPC authentication. By default, this is set to **`user:password`**.

- #### If you're using the **default configuration**:

  - You don't need to include these arguments.

- #### If you're using a **custom configuration**:
  - Pass your custom values using the `-r` and `-a` options, like this:

```bash
  $ ./taker -r 127.0.0.1:38332 -a myuser:mypass <SUBCOMMAND>
```

## For this tutorial, we'll assume a custom configuration with port 38332. Output examples will reflect this setup.

---

## Setting Up Your Wallet

### Generate a New Address

Before you can perform coinswaps, you need to fund your wallet. First, generate a new receiving address:

```bash
$ taker -r 127.0.0.1:38332 -a user:password get-new-address
```

**Output:**

```bash
bcrt1qyywgd4we5y7u05lnrgs8runc3j7sspwqhekrdd
```

This returns a new Bitcoin receiving address from the taker's wallet.

Now we can use the signet faucet to send some coins to this address. Use [this faucet](http://xjw3jlepdy35ydwpjuptdbu3y74gyeagcjbuyq7xals2njjxrze6kxid.onion/)(open in Tor browser) to get some signet coins.

### Check Wallet Balances

Once you have some coins in your wallet, you can check your balance by running the following command:

```bash
$ taker -r 127.0.0.1:38332 -a user:password get-balances
```

**Output:**

```json
{
  "contract": 0,
  "regular": 0,
  "spendable": 0,
  "swap": 0
}
```

The balance categories are explained as follows:
- **contract**: All live contract transaction balance locked in timelocks (if you see value here, you have unfinished or failed swaps)
- **regular**: All single signature regular wallet coins (seed balance)
- **spendable**: Total spendable amount in wallet (regular + swap balance)
- **swap**: All 2of2 multisig coins received in swaps

### List All UTXOs

To view all UTXOs in your wallet, use this command:

```bash
$ taker -r 127.0.0.1:38332 -a user:password list-utxo
```

**Output:**

```bash
```

This lists all UTXOs we know about along with their spend info. Since the wallet is empty, no UTXOs are displayed.

### List Regular UTXOs

To view only single signature wallet UTXOs, run:

```bash
$ taker -r 127.0.0.1:38332 -a user:password list-utxo-regular
```

**Output:**

```bash
```

This lists all single signature wallet UTXOs. These are all non-swap regular wallet UTXOs.

### List Swap UTXOs

To view UTXOs received from incoming swaps, run:

```bash
$ taker -r 127.0.0.1:38332 -a user:password list-utxo-swap
```

**Output:**

```bash
```

This lists all UTXOs received in incoming swaps. Since we haven't performed any coinswaps yet, this list is empty.

### List Contract UTXOs

To check for any locked funds from failed swaps, run:

```bash
$ taker -r 127.0.0.1:38332 -a user:password list-utxo-contract
```

**Output:**

```bash
```

This lists all UTXOs that we need to claim via timelock. If you see entries in this list, you should run the `recover` command to claim them.

### Fetch Available Offers

Now we are ready to initiate a coinswap. We are first going to sync the offer book to get a list of available makers:

```bash
$ taker -r 127.0.0.1:38332 -a user:password fetch-offers
```

**Output:**

```json
{
  "address": "m3qn53qt3wukwqvbx4yhq7v2qrsvf2oiddtjsqigwktzuuot33hmmuad.onion:8302",
  "amount_relative_fee_pct": 0.1,
  "base_fee": 100,
  "fidelity_bond": {
    "amount": 50000,
    "cert_expiry": 11,
    "conf_height": 12859,
    "lock_time": 25962,
    "outpoint": {
      "txid": "241a007ae754ac41b1910bead088b6f83a3bab2a4dc77118568d55e3a84dbe05",
      "vout": 0
    }
  },
  "max_size": 1949696,
  "min_size": 10000,
  "minimum_locktime": 20,
  "required_confirms": 1,
  "time_relative_fee_pct": 0.005,
  "timestamp": 1746899012,
  "tweakable_point": "0295dc108b6fe6036072f7db591736f69296a9312b1b95d986a8f88fa0672cd5d4"
}
```

This will fetch the list of available makers and display their offers.

### Initiate a Coinswap

Now we can initiate a coinswap with the makers:

```bash
$ taker -r 127.0.0.1:38332 -a user:password coinswap
```

This will initiate a coinswap with the default parameters. This will take some time. You can check swap progress at the log file in data directory. In a new terminal do `tail -f <datadir>/debug.log`.

### Recovering Failed Swaps

If a swap fails for any reason, the funds might be locked in a timelock contract. To check if you have any such locked funds, run:

```bash
$ taker -r 127.0.0.1:38332 -a user:password list-utxo-contract
```

**Output:**

```bash
```

If you see any UTXOs in the output, you can recover them using the `recover` command:

```bash
$ taker -r 127.0.0.1:38332 -a user:password recover
```

**Output:**

```bash
2025-08-13T14:36:38.734842084+05:30 INFO coinswap::wallet::api - Unfinished incoming txids: []
2025-08-13T14:36:38.734849418+05:30 INFO coinswap::wallet::api - Unfinished outgoing txids: []
2025-08-13T14:36:38.752510411+05:30 INFO coinswap::taker::api - Recovery completed.
```

This will attempt to recover all funds from failed swaps. In this case, since there are no unfinished transactions (both incoming and outgoing txids arrays are empty), the recovery process completes immediately with no funds to recover.

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
connection_type = "TOR"
```
 
- `control_port`: The Tor Control Port. Check the [tor doc](tor.md) for more details.
- `socks_port`: The Tor Socks Port. Check the [tor doc](tor.md) for more details.
- `tor_auth_password`: Optional password for Tor control authentication; empty by default.
- `connection_type`: The connection type to use for the directory server. Possible values are `CLEARNET` and `TOR`.

### Wallets

The taker uses wallet files to store the wallet data. The wallet files are stored in the `wallets` directory. These wallet files should be safely backed up as they contain the private keys to the wallet.
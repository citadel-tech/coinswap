# Taker Tutorial

The **Taker** is the party that initiates the coinswap. It discovers makers, requests offers from them and selects suitable makers for the swap.

In this tutorial, we will guide you through the process of setting up and running the taker, and conducting a coinswap.

## Setup

## Taker CLI

The taker CLI is an application that allows you to perform coinswaps as a taker.

> **Warning:**  
> Taker wallet files contain private keys required to spend your funds. Ensure these wallet files are backed up securely, and take appropriate measures to protect your private keys.

### Start Bitcoin Core (Pre-requisite)

`Taker` requires a **Bitcoin Core** RPC connection running on a **custom signet** for its operation(check [demo doc](./demo.md)).
> **Important:**  
> All apps are designed to run on our **custom signet** for testing purposes. The marketplace is only live in custom signet. Running the taker in other networks will not work as there's no marketplace in that network.

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
faucet for getting Testnet4 coins: http://xjw3jlepdy35ydwpjuptdbu3y74gyeagcjbuyq7xals2njjxrze6kxid.onion/

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

- The `-r` or `--ADDRESS:PORT` option specifies the Bitcoin Core RPC address and port. By default, this is set to `127.0.0.1:38332`.

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
$ taker get-new-address
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
$ taker get-balances
```

**Output:**

```json
{
  "contract": 0,
  "regular": 232560,
  "spendable": 251239,
  "swap": 18679
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
$ taker list-utxo
```

**Output:**

```bash
{
  "addr": "tb1qhfgd9u7y8usez37dl9uglv3s6wnugppmy2xeps",
  "amount": 18679,
  "confirmations": 2,
  "utxo_type": "swept-incoming-swap"
}
{
  "addr": "tb1qrsg2ls8exyzthjt2rsvkjhuag0a269867m3e0f",
  "amount": 232560,
  "confirmations": 3,
  "utxo_type": "regular"
}
```

This lists all UTXOs we know about along with their spend info. Since the wallet is empty, no UTXOs are displayed.

### List Regular UTXOs

To view only single signature wallet UTXOs, run:

```bash
$ taker list-utxo-regular
```

**Output:**

```bash
{
  "addr": "tb1qrsg2ls8exyzthjt2rsvkjhuag0a269867m3e0f",
  "amount": 232560,
  "confirmations": 3,
  "utxo_type": "regular"
}
```

This lists all single signature wallet UTXOs. These are all non-swap regular wallet UTXOs.

### List Swap UTXOs

To view UTXOs received from incoming swaps, run:

```bash
$ taker list-utxo-swap
```

**Output:**

```bash
{
  "addr": "tb1qhfgd9u7y8usez37dl9uglv3s6wnugppmy2xeps",
  "amount": 18679,
  "confirmations": 2,
  "utxo_type": "swept-incoming-swap"
}
```

This lists all UTXOs received in incoming swaps. Since we haven't performed any coinswaps yet, this list is empty.

### List Contract UTXOs

To check for any locked funds from failed swaps, run:

```bash
$ taker list-utxo-contract
```

**Output:**

```bash
```

This lists all UTXOs that we need to claim via timelock. If you see entries in this list, you should run the `recover` command to claim them.

### Fetch Available Offers

Now we are ready to initiate a coinswap. We are first going to sync the offer book to get a list of available makers:

```bash
$ taker fetch-offers
```

**Output:**

```json
{
  "base_fee": 100,
  "amount_relative_fee_pct": 0.1,
  "time_relative_fee_pct": 0.005,
  "required_confirms": 1,
  "minimum_locktime": 20,
  "max_size": 49949540,
  "min_size": 10000,
  "bond_outpoint": "21e3902cc0a2b94602fa94a7d3664f1a4d861df84af5049a334d5ddf402ed7f5:0",
  "bond_value": 904,
  "bond_expiry": 28864,
  "tor_address": "ewaexd2es2uzr34wp26cj5zgph7bug7znmmxolvwzmoeedbiyfgz3wqd.onion:8202"
}
```

This will fetch the list of available makers and display their offers.

### Initiate a Coinswap

Now we can initiate a coinswap with the makers:

```bash
$ taker coinswap
```

This will initiate a coinswap with the default parameters. The process typically takes several minutes to complete. You can monitor the swap progress by watching the debug log in a new terminal:

```bash
tail -f ~/.coinswap/taker/debug.log
```

```bash

2025-09-03T18:58:34.830814322+05:30 INFO coinswap::utill - ✅ Logger initialized successfully
2025-09-03T18:58:34.831885420+05:30 INFO coinswap::wallet::api - Wallet file at "/home/keraliss/.coinswap/taker/wallets/taker-wallet" successfully loaded.
2025-09-03T18:58:34.831932106+05:30 INFO coinswap::taker::config - Successfully loaded config file from : /home/keraliss/.coinswap/taker/config.toml
2025-09-03T18:58:34.832064049+05:30 INFO coinswap::utill - Tor is fully started and operational!
2025-09-03T18:58:34.832636855+05:30 INFO coinswap::taker::api - Succesfully loaded offerbook at : "/home/keraliss/.coinswap/taker/offerbook.json"
2025-09-03T18:58:34.832650756+05:30 INFO coinswap::wallet::rpc - Initializing wallet sync and save
2025-09-03T18:58:35.157963973+05:30 INFO coinswap::wallet::rpc - Completed wallet sync and save
2025-09-03T18:58:35.157987063+05:30 INFO coinswap::taker::api - Using regular UTXOs for coinswap
2025-09-03T18:58:35.157991757+05:30 INFO coinswap::taker::api - Syncing Offerbook
2025-09-03T18:58:35.158000831+05:30 INFO coinswap::taker::api - Fetching addresses from DNS: lp75qh3del4qot6fmkqq4taqm33pidvk63lncvhlwsllbwrl2f4g4qqd.onion:8080
2025-09-03T18:58:44.794069324+05:30 INFO coinswap::taker::routines - Downloading offer from ewaexd2es2uzr34wp26cj5zgph7bug7znmmxolvwzmoeedbiyfgz3wqd.onion:8202
2025-09-03T18:58:44.794123815+05:30 INFO coinswap::taker::routines - Downloading offer from rywnaguli5qwad2ayqyu3673acyyl5dw7bsjifhge4zohftfi76ybbid.onion:6102
2025-09-03T18:58:53.391435256+05:30 INFO coinswap::taker::routines - Downloaded offer from : rywnaguli5qwad2ayqyu3673acyyl5dw7bsjifhge4zohftfi76ybbid.onion:6102 
2025-09-03T18:58:53.641409491+05:30 INFO coinswap::taker::routines - Downloaded offer from : ewaexd2es2uzr34wp26cj5zgph7bug7znmmxolvwzmoeedbiyfgz3wqd.onion:8202 
2025-09-03T18:58:53.641555305+05:30 INFO coinswap::taker::api - Found offer from rywnaguli5qwad2ayqyu3673acyyl5dw7bsjifhge4zohftfi76ybbid.onion:6102. Verifying Fidelity Proof
2025-09-03T18:58:53.644582350+05:30 INFO coinswap::taker::api - Fidelity Bond verification success. Adding offer to our OfferBook
2025-09-03T18:58:53.644600038+05:30 INFO coinswap::taker::api - Found offer from ewaexd2es2uzr34wp26cj5zgph7bug7znmmxolvwzmoeedbiyfgz3wqd.onion:8202. Verifying Fidelity Proof
2025-09-03T18:58:53.645776479+05:30 INFO coinswap::taker::api - Fidelity Bond verification success. Adding offer to our OfferBook
2025-09-03T18:58:53.646061049+05:30 INFO coinswap::taker::api - Found 5 suitable makers for this swap round
2025-09-03T18:58:53.646074741+05:30 INFO coinswap::taker::api - Initiating coinswap with id : c874d9f7ac7e6230
2025-09-03T18:58:53.646081183+05:30 INFO coinswap::taker::api - Initializing First Hop.
2025-09-03T18:58:53.646089505+05:30 INFO coinswap::taker::api - Choosing next maker: 127.0.0.1:6102
2025-09-03T18:58:53.700488732+05:30 INFO coinswap::wallet::api - Address grouping: Selected 2 regular UTXOs (total: 253000 sats, target+fee: 20324 sats)
2025-09-03T18:58:53.702775316+05:30 INFO coinswap::wallet::spend - Adding change output with 232560 sats (fee: 440 sats)
2025-09-03T18:58:53.706404936+05:30 INFO coinswap::wallet::spend - Created Funding tx, txid: 3233bcef16eb6c2179fda31d9229c6989111a75f297c9c49859320069cd460e6 | Size: 220 vB | Fee: 440 sats | Feerate: 2.00 sat/vB
2025-09-03T18:58:53.706461956+05:30 INFO coinswap::wallet::funding - Created Funding tx, txid: 3233bcef16eb6c2179fda31d9229c6989111a75f297c9c49859320069cd460e6 | Size: 220 vB | Fee: 440 sats | Feerate: 2.00 sat/vB
2025-09-03T18:58:53.706768325+05:30 INFO wallet - created funding txes random amounts
2025-09-03T18:58:53.708215981+05:30 ERROR coinswap::taker::api - Failed to obtain sender's contract signatures from first_maker 127.0.0.1:6102: IO(Custom { kind: Other, error: "general SOCKS server failure" })
2025-09-03T18:58:53.708262825+05:30 INFO coinswap::taker::api - Choosing next maker: 127.0.0.1:6102
2025-09-03T18:58:53.756678424+05:30 INFO coinswap::wallet::api - Address grouping: Selected 2 regular UTXOs (total: 253000 sats, target+fee: 20324 sats)
2025-09-03T18:58:53.758990905+05:30 INFO coinswap::wallet::spend - Adding change output with 232560 sats (fee: 440 sats)
2025-09-03T18:58:53.762458743+05:30 INFO coinswap::wallet::spend - Created Funding tx, txid: db7afbbd4a7e7b0e1aa1b27b5f1deef509140413459b272c894a2ee8473eae44 | Size: 220 vB | Fee: 440 sats | Feerate: 2.00 sat/vB
2025-09-03T18:58:53.762508683+05:30 INFO coinswap::wallet::funding - Created Funding tx, txid: db7afbbd4a7e7b0e1aa1b27b5f1deef509140413459b272c894a2ee8473eae44 | Size: 220 vB | Fee: 440 sats | Feerate: 2.00 sat/vB
2025-09-03T18:58:53.762721044+05:30 INFO wallet - created funding txes random amounts
2025-09-03T18:58:53.763710980+05:30 ERROR coinswap::taker::api - Failed to obtain sender's contract signatures from first_maker 127.0.0.1:6102: IO(Custom { kind: Other, error: "general SOCKS server failure" })
2025-09-03T18:58:53.763757893+05:30 INFO coinswap::taker::api - Choosing next maker: 127.0.0.1:6102
2025-09-03T18:58:53.813457799+05:30 INFO coinswap::wallet::api - Address grouping: Selected 2 regular UTXOs (total: 253000 sats, target+fee: 20324 sats)
2025-09-03T18:58:53.815543924+05:30 INFO coinswap::wallet::spend - Adding change output with 232560 sats (fee: 440 sats)
2025-09-03T18:58:53.819894556+05:30 INFO coinswap::wallet::spend - Created Funding tx, txid: a680b1d032a5fa5febd4f2c38743c9aa657783c2d1c713b5748ba6c57c6474d0 | Size: 220 vB | Fee: 440 sats | Feerate: 2.00 sat/vB
2025-09-03T18:58:53.820038208+05:30 INFO coinswap::wallet::funding - Created Funding tx, txid: a680b1d032a5fa5febd4f2c38743c9aa657783c2d1c713b5748ba6c57c6474d0 | Size: 220 vB | Fee: 440 sats | Feerate: 2.00 sat/vB
2025-09-03T18:58:53.820375931+05:30 INFO wallet - created funding txes random amounts
2025-09-03T18:58:53.821413777+05:30 ERROR coinswap::taker::api - Failed to obtain sender's contract signatures from first_maker 127.0.0.1:6102: IO(Custom { kind: Other, error: "general SOCKS server failure" })
2025-09-03T18:58:53.821551711+05:30 INFO coinswap::taker::api - Choosing next maker: ewaexd2es2uzr34wp26cj5zgph7bug7znmmxolvwzmoeedbiyfgz3wqd.onion:8202
2025-09-03T18:58:53.872796787+05:30 INFO coinswap::wallet::api - Address grouping: Selected 2 regular UTXOs (total: 253000 sats, target+fee: 20324 sats)
2025-09-03T18:58:53.874726933+05:30 INFO coinswap::wallet::spend - Adding change output with 232560 sats (fee: 440 sats)
2025-09-03T18:58:53.877062518+05:30 INFO coinswap::wallet::spend - Created Funding tx, txid: 5eacac48877057bdda3e34d1a7e5f93d4d594cde599be166e465b5464501c76c | Size: 220 vB | Fee: 440 sats | Feerate: 2.00 sat/vB
2025-09-03T18:58:53.877090094+05:30 INFO coinswap::wallet::funding - Created Funding tx, txid: 5eacac48877057bdda3e34d1a7e5f93d4d594cde599be166e465b5464501c76c | Size: 220 vB | Fee: 440 sats | Feerate: 2.00 sat/vB
2025-09-03T18:58:53.877509748+05:30 INFO wallet - created funding txes random amounts
2025-09-03T18:58:54.568365133+05:30 INFO coinswap::taker::api - ===> ReqContractSigsForSender | ewaexd2es2uzr34wp26cj5zgph7bug7znmmxolvwzmoeedbiyfgz3wqd.onion:8202
2025-09-03T18:58:58.263137675+05:30 INFO coinswap::taker::api - <=== RespContractSigsForSender | ewaexd2es2uzr34wp26cj5zgph7bug7znmmxolvwzmoeedbiyfgz3wqd.onion:8202
2025-09-03T18:58:58.263583864+05:30 INFO coinswap::taker::api - Total Funding Txs Fees: 0.00000440 BTC
2025-09-03T18:58:58.263606558+05:30 INFO coinswap::taker::api - Transaction size: 220 vB (0.220 kvB)
2025-09-03T18:58:58.291022378+05:30 INFO coinswap::taker::api - Broadcasted Funding tx. txid: 5eacac48877057bdda3e34d1a7e5f93d4d594cde599be166e465b5464501c76c
2025-09-03T18:58:58.291105164+05:30 INFO coinswap::taker::api - Waiting for funding transaction confirmation. Txids : [5eacac48877057bdda3e34d1a7e5f93d4d594cde599be166e465b5464501c76c]
2025-09-03T18:58:58.292556173+05:30 INFO coinswap::taker::api - Funding tx Seen in Mempool. Waiting for confirmation for 0 secs
2025-09-03T18:58:59.062221277+05:30 INFO coinswap::taker::api - ===> WaitingFundingConfirmation | ewaexd2es2uzr34wp26cj5zgph7bug7znmmxolvwzmoeedbiyfgz3wqd.onion:8202
2025-09-03T18:59:29.063641719+05:30 INFO coinswap::taker::api - Funding tx Seen in Mempool. Waiting for confirmation for 30 secs
2025-09-03T18:59:29.683684462+05:30 INFO coinswap::taker::api - ===> WaitingFundingConfirmation | ewaexd2es2uzr34wp26cj5zgph7bug7znmmxolvwzmoeedbiyfgz3wqd.onion:8202

.
.
.
.
.

2025-09-03T19:55:19.045810783+05:30 INFO coinswap::wallet::api - Transaction seen in mempool,waiting for confirmation, txid: aed232df3ce3523434fcd2a8aad7fad02fa858d4be29cf0e1c18dde4a3cefb9d
2025-09-03T19:55:19.045831823+05:30 INFO coinswap::wallet::api - Next sync in 130 secs
2025-09-03T19:57:29.047058307+05:30 INFO coinswap::wallet::api - Transaction confirmed at blockheight: 16032, txid : aed232df3ce3523434fcd2a8aad7fad02fa858d4be29cf0e1c18dde4a3cefb9d
2025-09-03T19:57:29.047080134+05:30 INFO coinswap::wallet::api - Sweep Transaction aed232df3ce3523434fcd2a8aad7fad02fa858d4be29cf0e1c18dde4a3cefb9d confirmed at blockheight: 16032
2025-09-03T19:57:29.047090987+05:30 INFO coinswap::wallet::api - Successfully swept incoming swap coin, txid: aed232df3ce3523434fcd2a8aad7fad02fa858d4be29cf0e1c18dde4a3cefb9d
2025-09-03T19:57:29.047106047+05:30 INFO coinswap::wallet::api - Successfully removed incoming swaps coins
2025-09-03T19:57:29.047290187+05:30 INFO coinswap::taker::api - Successfully swept 1 incoming swap coins: [aed232df3ce3523434fcd2a8aad7fad02fa858d4be29cf0e1c18dde4a3cefb9d]
2025-09-03T19:57:29.047300886+05:30 INFO coinswap::taker::api - Successfully Completed Coinswap.
2025-09-03T19:57:29.047307582+05:30 INFO coinswap::taker::api - Shutting down taker.
2025-09-03T19:57:29.047631218+05:30 INFO coinswap::taker::api - offerbook data saved to disk.
2025-09-03T19:57:29.047740527+05:30 INFO coinswap::taker::api - Wallet data saved to disk.
```

### Recovering Failed Swaps

If a swap fails for any reason, the funds might be locked in a timelock contract. To check if you have any such locked funds, run:

```bash
$ taker list-utxo-contract
```

**Output:**

```bash
```

If you see any UTXOs in the output, you can recover them using the `recover` command:

```bash
$ taker  recover
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
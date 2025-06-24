# Maker-cli Tutorial

`maker-cli` is a straightforward command-line tool designed as an RPC client for `makerd`. It allows you to connect to the server, retrieve vital information, and manage various server operations efficiently.

In this guide, we'll walk you through how to use `maker-cli` to get the most out of your `makerd` setup. Let's get started!


> ### **Important Note**
> `makerd` listens to RPC requests from `maker-cli` **only** when it is fully set up. This setup includes creating a new fidelity bond (if one doesn't already exist) and completing other necessary configurations.  
> 
> If `makerd` is not fully set up, `maker-cli` commands will not function.  
> 
> 👉 **Before starting this tutorial**, ensure your `makerd` setup is complete.  
> If you're unsure how to set it up, check out our [Makerd Setup Guide](./makerd.md) first, and then return to this tutorial.
> 

---

## Getting Started with `maker-cli`

### View All Available Commands  
To see the full list of arguments and options available in `maker-cli`, run the following command:

```bash
$ ./maker-cli --help
```

This will display a detailed guide about the app and its capabilities.

#### **Output:**

```bash
coinswap 0.1.0
Developers at Citadel-Tech
A simple command line app to operate the makerd server.

The app works as a RPC client for makerd, useful to access the server, retrieve information, and
manage server operations.

For more detailed usage information, please refer:
https://github.com/citadel-tech/coinswap/blob/master/docs/maker-cli.md

This is early beta, and there are known and unknown bugs. Please report issues at:
https://github.com/citadel-tech/coinswap/issues

USAGE:
    maker-cli [OPTIONS] <SUBCOMMAND>

OPTIONS:
    -h, --help
            Print help information

    -p, --rpc-port <RPC_PORT>
            Sets the rpc-port of Makerd
            
            [default: 127.0.0.1:6103]

    -V, --version
            Print version information

SUBCOMMANDS:
    get-balances
            Get total wallet balances of different categories. regular: All single signature regular
            wallet coins (seed balance). swap: All 2of2 multisig coins received in swaps. contract:
            All live contract transaction balance locked in timelocks. If you see value in this
            field, you have unfinished or malfinished swaps. You can claim them back with recover
            command. fidelity: All coins locked in fidelity bonds. spendable: Spendable amount in
            wallet (regular + swap balance)
    get-new-address
            Gets a new bitcoin receiving address
    help
            Print this message or the help of the given subcommand(s)
    list-utxo
            Lists all utxos in the wallet. Including fidelity bonds
    list-utxo-contract
            Lists HTLC contract utxos
    list-utxo-fidelity
            Lists fidelity bond utxos
    list-utxo-swap
            Lists utxos received from incoming swaps
    send-ping
            Sends a ping to makerd. Will return a pong
    send-to-address
            Send Bitcoin to an external address and returns the txid
    show-data-dir
            Show the data directory path
    show-fidelity
            Show all the fidelity bonds, current and previous, with an (index, {bond_proof,is_spent}) tuple
    show-tor-address
            Show the server tor address
    stop
            Shutdown the makerd server
    sync-wallet
            Sync the maker wallet with current blockchain state
```

### Key Points About the `rpc-port` Argument
 - The `rpc-port` option specifies the RPC port that `makerd` listens on. By default, this is set to **`6103`**.

 - #### If you're using the **default configuration**:
   - You don't need to include the `rpc-port` argument.

 - #### If you're using a **custom configuration**:
   - Pass your custom port number using the `-p` or `--rpc-port` option, like this:

```bash
  $ ./maker-cli -p 6104 <SUBCOMMAND>
```

For this tutorial, we'll assume the default configuration is being used. Output examples will reflect this setup.
---

Here's a simplified and easier-to-read version of your content:

---

## Exploring Maker CLI Commands

### SendPing
To check if `makerd` is listening to RPC requests from `maker-cli`, use the `send-ping` command. 

Run:  
```bash
$ ./maker-cli send-ping
```

**Output:**  
```bash
success
```

This sends a ping to `makerd` and will return a pong, confirming that the maker server is listening and responding to requests.

---

### ShowDataDir
To get the maker server's data directory, use this command:

```bash
$ ./maker-cli show-data-dir
```

**Output:**  
```bash
<home_directory>/coinswap/maker
```

This is where all the maker's data is stored.

---

### ShowTorAddress
If your maker server is running on `Tor`, find its Tor address using this command:

```bash
$ ./maker-cli show-tor-address
```

**Output:**  
```bash
<maker's tor_address>
```

This shows the server tor address, which is our maker server's identity on the Tor network.

---

### ShowFidelity
When setting up `makerd`, we fund the maker's wallet and create a fidelity bond. To see details about our existing fidelity bond, use:

```bash
$ ./maker-cli show-fidelity
```

**Output:**  
```bash
{
    0: (
        FidelityBond {
            outpoint: OutPoint {
                txid: 6c06a925066b0cf8adb400e53001b20587729407bce7dcb95dcacd038950b0e4,
                vout: 0,
            },
            amount: 50000 SAT,
            lock_time: 2465 blocks,
            pubkey: PublicKey { ... },
            conf_height: 229349,
            cert_expiry: 5,
        },
        false,
    ),
}
```

This shows our maker's fidelity bond. 

> **Note:** Currently, a maker can have only one active (unexpired) fidelity bond at a time. Once a bond expires and is redeemed, a new fidelity bond can be created.


---

### ListFidelityUTXOs
To view fidelity UTXOs in the maker's wallet, run:

```bash
$ ./maker-cli list-utxo-fidelity
```

**Output:**  
```bash
[
    ListUnspentResultEntry {
        txid: 6c06a925066b0cf8adb400e53001b20587729407bce7dcb95dcacd038950b0e4,
        vout: 0,
        address: Some(
            Address<NetworkUnchecked>(BCRT1QKP92002WJPU5WFLU0YNTU3YXRVDR52N98STWFZJ7MF3F88RJ5PLQKDEUQY),
        ),
        amount: 50000 SAT,
        confirmations: 1,
        spendable: true,
    },
]
```

This lists fidelity bond UTXOs. Since only one live fidelity bond is allowed at a time, this shows a single UTXO of `50,000 sats`.

---

### CheckFidelityBalance
To check the balance of our fidelity UTXOs, use:

```bash
$ ./maker-cli get-balances
```

**Output:**  
```bash
{
    "regular": 1000000,
    "swap": 0,
    "contract": 0,
    "fidelity": 50000,
    "spendable": 1000000
}
```

This command shows the total wallet balances of different categories:
- **regular**: All single signature regular wallet coins (seed balance)
- **swap**: All 2of2 multisig coins received in swaps
- **contract**: All live contract transaction balance locked in timelocks. If you see value in this field, you have unfinished or malfinished swaps. You can claim them back with recover command
- **fidelity**: All coins locked in fidelity bonds
- **spendable**: Spendable amount in wallet (regular + swap balance)

This confirms the balance of our fidelity UTXOs matches the amount we set when creating the bond.

---

For more details about fidelity bonds, refer to the [Fidelity Bond Documentation](https://github.com/citadel-tech/Coinswap-Protocol-Specification/blob/main/v1/4_fidelity.md).

---

Next, we’ll explore other UTXOs and balances in Coinswap.


### Other utxos and their balance:
 #### Swap utxos:

 ```bash 
 $ ./maker-cli list-utxo-swap

 []
 ```

 This lists UTXOs received from incoming swaps. Since we have not done any coinswap yet, we have no swap utxos yet and thus we would have no swap balances as can be verified by running the command:

 ```bash
$ ./maker-cli get-balances

{
   "regular": 1000000,
   "swap": 0,
   "contract": 0,
   "fidelity": 50000,
   "spendable": 1000000
}
```


#### Contract utxos:

```bash
$ ./maker-cli list-utxo-contract

[]
```

This lists HTLC contract UTXOs. As mentioned above: We haven't participated in any coinswap transactions yet, so we don't have any unsuccessful coinswaps. Therefore, we have no `contract utxos` and no balance in this category, as shown:

```bash
$ ./maker-cli get-balances

{
    "regular": 1000000,
    "swap": 0,
    "contract": 0,
    "fidelity": 50000,
    "spendable": 1000000
}
```

> **IMPORTANT:**  
> We need to manually check UTXOs and their balances using the `list-utxo` and `get-balances` commands respectively.
> The `list-utxo` command returns all UTXOs present in the maker wallet, including the fidelity UTXOs.
> The `get-balances` command returns the total wallet balances of different categories, including normal UTXOs, swap UTXOs, contract UTXOs, fidelity UTXOs, and spendable UTXOs (normal + swap UTXOs).

Let's find them out: 

```bash
$ ./maker-cli list-utxo
[
    ListUnspentResultEntry {
        txid: 6c06a925066b0cf8adb400e53001b20587729407bce7dcb95dcacd038950b0e4,
        vout: 0,
        address: Some(
            Address<NetworkUnchecked>(BCRT1QKP92002WJPU5WFLU0YNTU3YXRVDR52N98STWFZJ7MF3F88RJ5PLQKDEUQY),
        ),
        label: Some(
            "ae28aba4",
        ),
        redeem_script: None,
        witness_script: None,
        script_pub_key: Script(OP_0 OP_PUSHBYTES_32 b04aa7bd4e90794727fc7926be44861b1a3a2a653c16e48a5eda62939c72a07e),
        amount: 50000 SAT,
        confirmations: 1,
        spendable: true,
        solvable: false,
        descriptor: None,
        safe: true,
    },
    ListUnspentResultEntry {
        txid: 6c06a925066b0cf8adb400e53001b20587729407bce7dcb95dcacd038950b0e4,
        vout: 1,
        address: Some(
            Address<NetworkUnchecked>(BCRT1QC538UUY77TN2YLYPTLXEQ6S8GL55753UK9C909),
        ),
        label: None,
        redeem_script: None,
        witness_script: None,
        script_pub_key: Script(OP_0 OP_PUSHBYTES_20 c5227e709ef2e6a27c815fcd906a0747e94f523c),
        amount: 949000 SAT,
        confirmations: 1,
        spendable: true,
        solvable: true,
        descriptor: Some(
            "wpkh([bd63c57a/1/0]024974169b3f59a123ac00e5034edd256593204cfab5668e5751d42bc864e0e955)#ljsywwyv",
        ),
        safe: true,
    },
]
```  

This lists all UTXOs in the wallet, including fidelity bonds. We created a funding transaction to fund the maker wallet and establish the fidelity bonds. As a result, the command displays two UTXOs: 

1. The **fidelity UTXO** (which we've already seen).
2. The **normal funding UTXO**.

### Breakdown:
- Initially, we funded the wallet with `0.01 BTC`.
- `50,000 sats` were used for the fidelity bond.
- `1,000 sats` were used as the mining fee for the fidelity transaction.

The remaining balance after these transactions is:

**949,000 sats** = **1,000,000 sats** (total funding) - **50,000 sats** (for the fidelity bond) - **1,000 sats** (mining fees).

We can verify this balance by running the `get-balances` command, which shows the total wallet balances of different categories:

```bash
$ ./maker-cli get-balances
{
    "regular": 949000,
    "swap": 0,
    "contract": 0,
    "fidelity": 50000,
    "spendable": 949000
}
```

---

### Deriving an Address from the Maker's Wallet:
To derive a new external address from the maker's wallet, use the `get-new-address` command with `maker-cli`.

```bash
$ ./maker-cli get-new-address

<maker's external address>
```

This gets a new bitcoin receiving address from the maker's wallet.

### Spending `10,000 sats` from the Maker's Wallet:
Next, let's send `10,000 sats` from the maker's wallet to an external address.

#### **Step 1**: Derive an External Address Using `bitcoin-cli`'s `getnewaddress` Command

```bash
$ bitcoin-cli getnewaddress
```

#### **Step 2**: Use `maker-cli`'s `send-to-address` Command to Send the Amount to the Derived Address

The `send-to-address` command allows us to send Bitcoin to an external address. To view the available options for this command, run the `--help` option:

```bash
$ ./maker-cli send-to-address --help

Send Bitcoin to an external address and returns the txid

USAGE:
    maker-cli send-to-address --address <ADDRESS> --amount <AMOUNT> --fee <FEE>

OPTIONS:
    -a, --amount <AMOUNT>      Amount to send in sats
    -f, --fee <FEE>            Total fee to be paid in sats
    -h, --help                 Print help information
    -t, --address <ADDRESS>    Recipient's address
```


> **Note:**  
> The command currently requires the `fee` parameter to specify the total mining fee for the transaction instead of using `fee_rate`. This is because the functionality to calculate the fee using a `fee_rate` for transactions that have not been created yet has not been implemented. This process will be improved in the next release.


Let's now send `10,000 sats` to the derived address, with a mining fee of `1,000 sats`:

```bash
$ ./maker-cli send-to-address --amount 10000 --address <derived address> --fee 1000

<tx hex>
```

This command will create a transaction, send `10,000 sats` from the maker's wallet to the derived address, broadcast the transaction to the network, and return the transaction ID in hex format.

### Transaction Confirmation and Wallet Synchronization:

Once the transaction is broadcasted to the network, it will need to be confirmed. After confirmation, we have to sync our wallet to catch the latest updates:

```bash
$ ./maker-cli sync-wallet
success
```

This syncs the maker wallet with current blockchain state. On `makerd`, we will see:

```bash
INFO coinswap::maker::rpc::server - Starting wallet sync.
INFO coinswap::maker::rpc::server - Wallet sync success.
```

### Checking Wallet Balances and UTXOs:
Finally, we can check the wallet's updated balances and the list of UTXOs as done previously.

---

### **Fidelity UTXOs**:
```bash
$ ./maker-cli list-utxo-fidelity

[
    ListUnspentResultEntry {
        txid: 6c06a925066b0cf8adb400e53001b20587729407bce7dcb95dcacd038950b0e4,
        vout: 0,
        address: Some(
            Address<NetworkUnchecked>(BCRT1QKP92002WJPU5WFLU0YNTU3YXRVDR52N98STWFZJ7MF3F88RJ5PLQKDEUQY),
        ),
        label: Some(
            "ae28aba4",
        ),
        redeem_script: None,
        witness_script: None,
        script_pub_key: Script(OP_0 OP_PUSHBYTES_32 b04aa7bd4e90794727fc7926be44861b1a3a2a653c16e48a5eda62939c72a07e),
        amount: 50000 SAT,
        confirmations: 2,
        spendable: true,
        solvable: false,
        descriptor: None,
        safe: true,
    },
]

$ ./maker-cli get-balances

{
    "regular": 949000,
    "swap": 0,
    "contract": 0,
    "fidelity": 50000,
    "spendable": 949000
}
```  

> **NOTE**: Fidelity UTXOs are not used for spending purposes. We can only spend these UTXOs by using the `redeem_fidelity` command after the fidelity bond expires. This is why the UTXO list and balance remain unchanged.

---

### **Swap UTXOs**:
```bash
$ ./maker-cli list-utxo-swap
[]

$ ./maker-cli get-balances
{
    "regular": 949000,
    "swap": 0,
    "contract": 0,
    "fidelity": 50000,
    "spendable": 949000
}
```

---

### **Contract UTXOs**:
```bash
$ ./maker-cli list-utxo-contract
[]

$ ./maker-cli get-balances
{
    "regular": 949000,
    "swap": 0,
    "contract": 0,
    "fidelity": 50000,
    "spendable": 949000
}
```

---

### **Total UTXOs**:
```bash
$ ./maker-cli list-utxo

[
    ListUnspentResultEntry {
        txid: 21de4b89c37e495d05161ed81690079b257ff5776150171740bf34e8b9163cd1,
        vout: 1,
        address: Some(
            Address<NetworkUnchecked>(BCRT1QVCRZ5QPGJCX25WASWSA4Z8MZS8WUZYX6FNQ60L),
        ),
        label: None,
        redeem_script: None,
        witness_script: None,
        script_pub_key: Script(OP_0 OP_PUSHBYTES_20 66062a0028960caa3bb0743b511f6281ddc110da),
        amount: 938000 SAT,
        confirmations: 1,
        spendable: true,
        solvable: true,
        descriptor: Some(
            "wpkh([bd63c57a/1/1]03aa76bd9dd512adbfea796d65d1bda2e7ed691b6c28cfa630991c8cb99db16fa9)#8e495hwg",
        ),
        safe: true,
    },
    ListUnspentResultEntry {
        txid: 6c06a925066b0cf8adb400e53001b20587729407bce7dcb95dcacd038950b0e4,
        vout: 0,
        address: Some(
            Address<NetworkUnchecked>(BCRT1QKP92002WJPU5WFLU0YNTU3YXRVDR52N98STWFZJ7MF3F88RJ5PLQKDEUQY),
        ),
        label: Some(
            "ae28aba4",
        ),
        redeem_script: None,
        witness_script: None,
        script_pub_key: Script(OP_0 OP_PUSHBYTES_32 b04aa7bd4e90794727fc7926be44861b1a3a2a653c16e48a5eda62939c72a07e),
        amount: 50000 SAT,
        confirmations: 2,
        spendable: true,
        solvable: false,
        descriptor: None,
        safe: true,
    },
]

$ ./maker-cli get-balances
{
    "regular": 938000,
    "swap": 0,
    "contract": 0,
    "fidelity": 50000,
    "spendable": 938000
}
```

---
### **Redeem Fidelity**:
[TODO]

### **Shutting Down Maker Server**:

After performing all functionalities, we can stop the maker server using the `stop` command.

```bash
$ ./maker-cli stop

Shutdown Initiated
```

This shuts down the makerd server. Once you run this command, the maker server initiates a shutdown, and we'll see the following logs indicating the shutdown process:

```bash
 INFO coinswap::maker::server - [6102] Maker is shutting down.
 INFO coinswap::maker::api - Joining 4 threads
 INFO coinswap::maker::api - [6102] Thread RPC Thread joined
 INFO coinswap::maker::api - [6102] Thread Contract Watcher Thread joined
 INFO coinswap::maker::api - [6102] Thread Idle Client Checker Thread joined
 INFO coinswap::maker::api - [6102] Thread Bitcoin Core Connection Checker Thread joined
 INFO coinswap::maker::api - Successfully joined 4 threads
 INFO coinswap::maker::server - Shutdown wallet sync initiated.
 INFO coinswap::maker::server - Shutdown wallet syncing completed.
 INFO coinswap::maker::server - Wallet file saved to disk.
 INFO coinswap::maker::server - Maker Server is shut down successfully
```

---

And that's it! Now you are ready to be a maker in the Coinswap network. Start your maker servers, perform coinswaps, and enjoy earning fees from takers who participate in coinswaps with you.



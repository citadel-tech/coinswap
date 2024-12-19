# Taker-Cli Tutorial

## What is the Taker in Coinswap?

A **taker** initiates a coinswap transaction, seeking to exchange their coins with the assistance of **makers**, without losing control of their funds.

## Prerequisites

You will need to have `bitcoind` set up before proceeding with the steps below.

### Start Bitcoind

Make sure you start your `bitcoind` daemon to work with the `taker-cli` by running the following command:

```bash
bitcoind -daemon
```

## Create a Wallet

To perform basic operations such as generating new blocks, you'll need a wallet. Let's create a wallet named `default`.

```bash
bitcoin-cli createwallet "default"
```

## Send Funds to the `default` Wallet by Generating Blocks

### Step 1: Get a New Address

First, get a new address from the `default` wallet.

```bash
bitcoin-cli getnewaddress
```

### Step 2: Generate 101 Blocks and Send Funds

Generate 101 blocks and send the funds to the address generated in Step 1.

```bash
bitcoin-cli generatetoaddress 101 <address>
```

## About the Taker Wallet

Currently, there is no specific command to initialize a new taker wallet or load an existing one. Each command automatically loads the wallet from the specified data directory or creates a new one if it doesn't exist.

## Fund 1 BTC to the Taker Wallet

### Step 1: Get a New Address from the Taker Wallet

```bash
./taker -a <USER:PASSWORD> -r <ADDRESS:PORT> get-new-address
```

### Step 2: Send 1 BTC from the Default Wallet to the Taker Wallet

```bash
bitcoin-cli -rpcwallet="default" sendtoaddress <address> 1
```

### Step 3: Generate a Block to Confirm the Transaction

```bash
bitcoin-cli -rpcwallet="default" getnewaddress
bitcoin-cli generatetoaddress 1 <address>
```

## Check the Taker Wallet Balance

After funding, you can check different types of balances in the `taker` wallet.

### Seed Balance

This represents the available balance received by the `default` wallet.

```bash
./taker -a <USER:PASSWORD> -r <ADDRESS:PORT> seed-balance
```

### Contract Balance

Currently, this will show 0 SAT as there are no contracts yet.

```bash
./taker -a <USER:PASSWORD> -r <ADDRESS:PORT> contract-balance
```

### Swap Balance

This will also show 0 SAT for now, as no swaps have been initiated.

```bash
./taker -a <USER:PASSWORD> -r <ADDRESS:PORT> swap-balance
```

### Total Balance

The total balance is the sum of seed, contract, and swap balances. Currently, it will be equal to the seed balance.

```bash
./taker -a <USER:PASSWORD> -r <ADDRESS:PORT> total-balance
```

## Check UTXOs in the Taker Wallet

You can check the UTXOs in the `taker` wallet as follows:

### Seed UTXO

The seed UTXO will reflect the 1 BTC sent earlier.

```bash
./taker -a <USER:PASSWORD> -r <ADDRESS:PORT> seed-utxo
```

### Contract UTXO

This will return an empty vector as there are no contracts yet.

```bash
./taker -a <USER:PASSWORD> -r <ADDRESS:PORT> contract-utxo
```

### Swap UTXO

Similar to the contract UTXO, this will also return an empty vector for now.

```bash
./taker -a <USER:PASSWORD> -r <ADDRESS:PORT> swap-utxo
```

## Send 0.5 BTC from the `taker` Wallet to the `default` Wallet

### Step 1: Get an Address from the `Default` Wallet

```bash
bitcoin-cli -rpcwallet="default" getnewaddress
```

### Step 2: Send Funds Using the `send-to-address` Command

Call the `send-to-address` command with the following parameters:

- `amount`: Amount to be sent
- `address`: Recipient address
- `fee`: Fee for the transaction being created.

> **Note:**
> Currently, the command requires the `fee` parameter instead of `fee_rate` because it's not possible to calculate the fee for a transaction that hasn't been created yet using only a `fee_rate`.
> When the `fee` is provided, the command will return two outputs:
>
> 1. The hex of the signed transaction (which is not yet broadcasted).
> 2. The calculated `fee_rate`.
>    Having the calculated `fee_rate` allows us to infer the necessary fee for a successful transaction. Based on this, we can adjust the `fee` parameter and determine the appropriate fee required.
>    While this process might seem unconventional, it will be improved with future BDK integration.

### Step 3: Execute the `send-to-address` Command

To get the hex of the signed transaction and the calculated `fee_rate` from the given `fee`, use the `send-to-address` command.
For example, with a `fee` of 1000 SAT:

```bash
./taker -a <USER:PASSWORD> -r <ADDRESS:PORT> send-to-address <address> 50000000 1000
```

### Step 4: Broadcast the Signed Transaction

After receiving the transaction hex, broadcast the signed transaction to the network using:

```bash
bitcoin-cli sendrawtransaction <tx hex>
```

### Step 5: Generate a Block to Confirm the Transaction

Generate a block using the `default` wallet to confirm the transaction. After this, 0.5 BTC will be successfully spent from the `taker` wallet to the `default` wallet.

```bash
bitcoin-cli generatetoaddress 1 <address>
```

## Check the Balance and UTXOs of the Taker Wallet

- **Seed Balance**:
  After spending, the seed balance will be 49,999,000 SAT, calculated as:
  `(Initial balance (1 BTC) - spend amount (0.5 BTC) - fee (1000 SAT))`

- **Total Balance**:
  The total balance will be the same as the seed balance.

- **Swap Balance and Contract Balance**:
  Both of these will remain at 0 SAT, as no swaps or contracts have been initiated.


<!-- TODO: Execute a swap -->

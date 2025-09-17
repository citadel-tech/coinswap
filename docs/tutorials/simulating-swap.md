# Simulating a Regular Swap With 2 Makers and 1 Taker

## Start Bitcoin Core on Custom Signet

Ensure your `bitcoind` instance is running and configured for `custom signet`. If you followed the [demo setup guide](../demo.md), you can start it as follows:

```bash
bitcoind
```

## Makers Setup

Set up two independent maker nodes. Each maker requires its own data directory and unique RPC/network ports to avoid conflicts.

To view all available `makerd` options:

```bash
makerd --help
```

For more detailed information, refer to `docs/makerd.md`.

### Maker 1 Setup

1. **Start Maker 1**
   Start the first maker daemon. You will be prompted to create an encryption passphrase for its wallet, and the logs will display the wallet address.

```bash
makerd -d .coinswap/maker-one -w maker-one
OR
makerd -a user:password -d .coinswap/maker-one -w maker-one -r <127.0.0.1:port>
```

2. **Configure Maker 1 (optional)**
   Edit the configuration file to customize the fidelity bond amount and timelock period. Restart makerd to apply the changes.

```bash
nano $HOME/.coinswap/maker-one/config.toml
```

3. **Fund Maker 1**
   Fund the maker's wallet using the address displayed in the console output. The [Custom Signet Faucet](http://s2ncekhezyo2tkwtftti3aiukfpqmxidatjrdqmwie6xnf2dfggyscad.onion/) is accessible via the Tor browser. Ensure you send enough funds to cover the fidelity bond and transaction fees.

Output:

```bash
2025-08-29T20:16:23.418640097+05:30 INFO coinswap::maker::server - No active Fidelity Bonds found. Creating one.
...
2025-08-29T20:16:23.427692286+05:30 INFO coinswap::maker::server - Send at least 0.00050324 BTC to tb1qu385x7dr7fx73ua78ffdyq37vvj7f8fsxd26p8 | If you send extra, that will be added to your wallet balance
```

> **Note:** In the log examples, `...` indicates omitted log lines to keep the example short.

After sufficient funds are received and confirmed, `makerd` automatically creates the fidelity bond

Output:

```bash
2025-08-29T20:21:03.558348147+05:30 INFO coinswap::wallet::api - Selected 1 regular UTXOs
2025-08-29T20:21:03.560301107+05:30 INFO coinswap::wallet::spend - Adding change output with 75564 sats (fee: 354 sats)
2025-08-29T20:21:03.560913632+05:30 INFO coinswap::wallet::spend - Created Funding tx, txid: d00fba5e115e387ee3872a308a690deb093c211a7a4df04e7c825c04f78b428e | Size: 178 vB | Fee: 354 sats | Feerate: 1.99 sat/vB
2025-08-29T20:21:03.586657773+05:30 INFO coinswap::wallet::api - Transaction seen in mempool,waiting for confirmation, txid: d00fba5e115e387ee3872a308a690deb093c211a7a4df04e7c825c04f78b428e
2025-08-29T20:21:03.586726465+05:30 INFO coinswap::wallet::api - Next sync in 10 secs
2025-08-29T20:21:13.591821200+05:30 INFO coinswap::wallet::api - Transaction seen in mempool,waiting for confirmation, txid: d00fba5e115e387ee3872a308a690deb093c211a7a4df04e7c825c04f78b428e
```

Wait until the transaction is confirmed. You can monitor its status using the provided `txid` on the [Custom Signet Explorer](http://xlrj7ilheypw67premos73gxlcl7ha77kbhrqys7mydp7jve25olsxyd.onion/) (accessible via Tor).
After confirmation, the fidelity bond is created.

Output:

```bash
2025-08-29T21:11:03.602222774+05:30 INFO coinswap::wallet::api - Transaction confirmed at blockheight: 99649, txid : d00fba5e115e387ee3872a308a690deb093c211a7a4df04e7c825c04f78b428e
2025-08-29T21:11:03.602293774+05:30 INFO coinswap::wallet::rpc - Initializing wallet sync and save
2025-08-29T21:11:03.772161931+05:30 INFO coinswap::wallet::rpc - Completed wallet sync and save
2025-08-29T21:11:03.772629331+05:30 INFO coinswap::maker::server - [6102] Successfully created fidelity bond
...
2025-08-29T21:11:06.995136026+05:30 INFO coinswap::maker::server - [6102] Server Setup completed!! Use maker-cli to operate the server and the internal wallet.
2025-08-29T21:11:33.904427980+05:30 INFO coinswap::wallet::fidelity - Fidelity Bond found | Index: 0 | Bond Value : 0.00000029 BTC
```

4. **Interact with Maker 1**
   After the fidelity bond is created, you can interact with `makerd` using `maker-cli`.

```bash
maker-cli get-balances
{
  "contract": 0,
  "fidelity": 50000,
  "regular": 75564,
  "spendable": 75564,
  "swap": 0
}
```

### Maker 2 Setup

1. **Configure Maker 2 ports**
   When running multiple makers on a single system, their default network port (`6102`) and RPC ports (`6104`) will clash. You must update the configuration for Maker 2.

Run Maker 2 once to generate the default config.toml, then exit `makerd` using `CTRL+C`.

```bash
makerd -a user:password -d .coinswap/maker-two -w maker-two -r <address:port>
OR
makerd -d .coinswap/maker-two -w maker-two
```

Edit the `config.toml` file for Maker 2:

```bash
nano .coinswap/maker-two/config.toml
```

Update the following fields:

```bash
# Maker Configuration File
# Network port for client connections
network_port = 6104
# RPC port for maker-cli operations
rpc_port = 6105
```

> **Note:** You can use `ss -tulnp` to check for active ports and avoid conflicts.

2. **Start Maker 2**
   Start the second maker daemon with its unique data directory and updated ports.

```bash
makerd -a user:password -d ./coinswap/maker-two -w maker-two -r <address:port>
OR
makerd -d ./coinswap/maker-two -w maker-two
```

3. **Fund and initialize Maker 2**
   Similar to Maker 1:
   1. Fund the wallet address displayed in Maker 2's console output using the [Custom Signet Faucet](http://s2ncekhezyo2tkwtftti3aiukfpqmxidatjrdqmwie6xnf2dfggyscad.onion/).
   2. Wait for transaction confirmations on the `custom signet` blockchain.
   3. Verify that the fidelity bond is created successfully in the console logs.

```bash
2025-08-29T22:19:05.861976862+05:30 INFO coinswap::maker::server - [6104] Server Setup completed!! Use maker-cli to operate the server and the internal wallet.
2025-08-29T22:18:42.112734371+05:30 INFO coinswap::wallet::fidelity - Fidelity Bond found | Index: 0 | Bond Value : 0.00000029

# Verify bond using maker-cli
$ maker-cli -p 127.0.0.1:6105 get-balances
{
  "contract": 0,
  "fidelity": 50000,
  "regular": 121342,
  "spendable": 121342,
  "swap": 0
}
```

Maker 2 is now ready to participate in swaps.

### Set Up and Execute the Taker Swap

#### Set Up Taker

Generate a new funding address. The default taker directory is `$HOME/.coinswap/taker`.

```bash
 taker get-new-address
 tb1qdnk0q48guzc3m3js004jdyds94pklq47x6pduy
```

#### Fund the Taker

Fund the taker's wallet using the address obtained in the previous step. Use the [Custom Signet Faucet](http://s2ncekhezyo2tkwtftti3aiukfpqmxidatjrdqmwie6xnf2dfggyscad.onion/). send enough funds to cover the swap amount and transaction fees.

Verify taker balances after funding:

```bash
taker get-balances
```

Output:

```json
{
  "contract": 0,
  "regular": 677071,
  "spendable": 677071,
  "swap": 0
}
```

#### Fetch Offers

This command fetches offers from available makers in the market and stores them in the offer book (`.coinswap/taker/offerbook.json`).

```bash
taker fetch-offers
```

Output:

```bash
...
2025-08-30T00:58:08.415899890+05:30 INFO coinswap::taker::api - Found offer from 127.0.0.1:6102. Verifying Fidelity Proof
2025-08-30T00:58:08.423967990+05:30 INFO coinswap::taker::api - Fidelity Bond verification success. Adding offer to our OfferBook
2025-08-30T00:58:08.424036190+05:30 INFO coinswap::taker::api - Found offer from 127.0.0.1:6104. Verifying Fidelity Proof
2025-08-30T00:58:08.426471090+05:30 INFO coinswap::taker::api - Fidelity Bond verification success. Adding offer to our OfferBook
{
  "base_fee": 1000,
  "amount_relative_fee_pct": 2.5,
  "time_relative_fee_pct": 0.1,
  "required_confirms": 1,
  "minimum_locktime": 20,
  "max_size": 75564,
  "min_size": 10000,
  "bond_outpoint": "d00fba5e115e387ee3872a308a690deb093c211a7a4df04e7c825c04f78b428e:0",
  "bond_value": 28,
  "bond_expiry": 100595,
  "tor_address": "127.0.0.1:6102"
}
...
```

#### Execute the Coinswap

Once offers are fetched, you can initiate a coinswap. The `taker coinswap` command allows you to specify the swap amount and the number of makers.

- `-a, --amount [AMOUNT]`: Sets the swap amount in sats (default: `20000` sats).
- `-m, --makers [MAKERS]`: Sets the maker count to swap with. Swapping with less than 2 makers is not allowed to maintain client privacy. Adding more makers in the swap will incur more swap fees (default: `2`).

```bash
taker coinswap --amount 20000 --makers 2
```

Initializing First Hop
Output:

```bash
...
2025-08-30T01:25:11.587410848+05:30 INFO coinswap::taker::api - Found 2 suitable makers for this swap round
2025-08-30T01:25:11.587481648+05:30 INFO coinswap::taker::api - Initiating coinswap with id : 54fc81645a9808d0
2025-08-30T01:25:11.587535548+05:30 INFO coinswap::taker::api - Initializing First Hop.
2025-08-30T01:25:11.587596648+05:30 INFO coinswap::taker::api - Choosing next maker: 127.0.0.1:6102
2025-08-30T01:25:11.754258549+05:30 INFO coinswap::wallet::api - Address grouping: Selected 2 regular UTXOs (total: 677071 sats, target+fee: 20324 sats)
2025-08-30T01:25:11.755999849+05:30 INFO coinswap::wallet::spend - Adding change output with 656631 sats (fee: 440 sats)
...
2025-08-30T01:25:11.757332049+05:30 INFO coinswap::taker::api - ===> ReqContractSigsForSender | 127.0.0.1:6102
2025-08-30T01:25:12.418760455+05:30 INFO coinswap::taker::api - <=== RespContractSigsForSender | 127.0.0.1:6102
2025-08-30T01:25:12.419252155+05:30 INFO coinswap::taker::api - Total Funding Txs Fees: 0.00000440 BTC
2025-08-30T01:25:12.419278055+05:30 INFO coinswap::taker::api - Transaction size: 220 vB (0.220 kvB)
2025-08-30T01:25:12.448798755+05:30 INFO coinswap::taker::api - Broadcasted Funding tx. txid: a72091d5eeafe6915b023107416217ccfc2c49402c173192b046327bf10fee85
2025-08-30T01:25:12.448891655+05:30 INFO coinswap::taker::api - Waiting for funding transaction
```

Wait for the funding transaction to be confirmed. After confirmation, the taker will proceed with the next hop.

Output:

```bash
a72091d5eeafe6915b023107416217ccfc2c49402c173192b046327bf10fee85 | Confirmed at 1
2025-08-30T02:43:30.294218569+05:30 INFO coinswap::taker::api - Connecting to 127.0.0.1:6102 | Send Sigs Init Next Hop
2025-08-30T02:44:57.745290283+05:30 INFO coinswap::taker::api - ===> ProofOfFunding | 127.0.0.1:6102
2025-08-30T02:44:57.745333683+05:30 INFO coinswap::taker::api - Fundix Txids: [a72091d5eeafe6915b023107416217ccfc2c49402c173192b046327bf10fee85]
2025-08-30T02:44:57.988673984+05:30 INFO coinswap::taker::routines - Maker Received = 0.00020000 BTC | Maker is Forwarding = 0.00017260 BTC |  Coinswap Fees = 0.00002300 BTC
2025-08-30T02:44:57.988719484+05:30 INFO coinswap::taker::api - <=== ReqContractSigsAsRecvrAndSender | 127.0.0.1:6102
2025-08-30T02:44:58.137767685+05:30 INFO coinswap::taker::api - ===> ReqContractSigsForSender | 127.0.0.1:6104
2025-08-30T02:44:58.716216190+05:30 INFO coinswap::taker::api - <=== RespContractSigsForSender | 127.0.0.1:6104
2025-08-30T02:44:58.716350090+05:30 INFO coinswap::taker::api - Taker is previous peer. Signing Receivers Contract Txs
2025-08-30T02:44:58.716861890+05:30 INFO coinswap::taker::api - ===> RespContractSigsForRecvrAndSender | 127.0.0.1:6102
2025-08-30T02:44:58.717058190+05:30 INFO coinswap::taker::api - Waiting for funding transaction confirmation. Txids : [b0223aac017dba5480a13f74d19090188637c86cd954c00105199ff1b1cbba47]
2025-08-30T02:44:58.726610890+05:30 INFO coinswap::taker::api - Waiting for funding tx to appear in mempool | 0 secs
...
```

Final hop and sweep completion:

```bash
...
2025-08-30T03:19:45.140704175+05:30 INFO coinswap::taker::api - Tx 42607e39c9c4c5f5972595ad2c74164f5dea43e836cdead0b630cd5c7034e2fb | Confirmed at 1
2025-08-30T03:19:45.144089575+05:30 INFO coinswap::wallet::rpc - Initializing wallet sync and save
2025-08-30T03:19:45.381942277+05:30 INFO coinswap::wallet::rpc - Completed wallet sync and save
2025-08-30T03:19:48.536561116+05:30 INFO coinswap::taker::api - ===> ReqContractSigsForRecvr | 127.0.0.1:6104
2025-08-30T03:24:53.055859144+05:30 WARN coinswap::taker::api - Failed to connect to maker 127.0.0.1:6104 to request signatures for receiver, reattempting, 1 of 10 |  error=Net(IO(Os { code: 11, kind: WouldBlock, message: "Resource temporarily unavailable" }))
2025-08-30T03:24:54.057118208+05:30 INFO coinswap::taker::api - ===> ReqContractSigsForRecvr | 127.0.0.1:6104
2025-08-30T03:26:58.877750262+05:30 INFO coinswap::taker::api - <=== RespContractSigsForRecvr | 127.0.0.1:6104
2025-08-30T03:27:01.871558700+05:30 INFO coinswap::taker::api - ===> HashPreimage | 127.0.0.1:6102
2025-08-30T03:27:01.879753600+05:30 INFO coinswap::taker::api - <=== PrivateKeyHandover | 127.0.0.1:6102
2025-08-30T03:27:01.880016800+05:30 INFO coinswap::taker::api - ===> PrivateKeyHandover | 127.0.0.1:6102
2025-08-30T03:27:04.880540039+05:30 INFO coinswap::taker::api - ===> HashPreimage | 127.0.0.1:6104
2025-08-30T03:27:04.881710439+05:30 INFO coinswap::taker::api - <=== PrivateKeyHandover | 127.0.0.1:6104
2025-08-30T03:27:04.881915739+05:30 INFO coinswap::taker::api - ===> PrivateKeyHandover | 127.0.0.1:6104
2025-08-30T03:27:04.882043839+05:30 INFO coinswap::taker::api - Swaps settled successfully. Sweeping the coins and reseting everything.
...
2025-08-30T03:27:06.361030450+05:30 INFO coinswap::wallet::api - Transaction seen in mempool,waiting for confirmation, txid: da79d2e415579982de3a93fb612fd0bf34094baa5e707317847cfdd546907f39
...
```

Sweep complete:

```bash
2025-08-30T06:36:03.594637029+05:30 INFO coinswap::wallet::api - Sweep Transaction da79d2e415579982de3a93fb612fd0bf34094baa5e707317847cfdd546907f39 confirmed at blockheight: 99720
2025-08-30T06:36:03.594651329+05:30 INFO coinswap::wallet::api - Successfully swept incoming swap coin, txid: da79d2e415579982de3a93fb612fd0bf34094baa5e707317847cfdd546907f39
2025-08-30T06:36:03.594665129+05:30 INFO coinswap::wallet::api - Successfully removed incoming swaps coins
2025-08-30T06:36:03.594878529+05:30 INFO coinswap::taker::api - Successfully swept 1 incoming swap coins: [da79d2e415579982de3a93fb612fd0bf34094baa5e707317847cfdd546907f39]
2025-08-30T06:36:03.594933329+05:30 INFO coinswap::taker::api - Successfully Completed Coinswap.
2025-08-30T06:36:03.594948629+05:30 INFO coinswap::taker::api - Shutting down taker.
2025-08-30T06:36:03.595819629+05:30 INFO coinswap::taker::api - offerbook data saved to disk.
2025-08-30T06:36:03.595989629+05:30 INFO coinswap::taker::api - Wallet data saved to disk.
```

## Troubleshooting

- **RPC connection issues**: If you encounter "connection refused" errors, verify that `bitcoind` is running, the RPC port is correct (e.g., 38332 for `signet`).
- **Port clashes**: When running multiple `makerd` or `taker` instances on the same machine, ensure each uses unique `network_port` and `rpc_port` values in their respective `config.toml` files. Use `ss -tulnp` to inspect active ports.

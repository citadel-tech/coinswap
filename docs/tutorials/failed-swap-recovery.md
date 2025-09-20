# Simulating a Failed Swap and Triggering Recovery

After the taker has broadcasted the first funding transaction and is waiting for it to be confirmed, stop any one maker. This action will cause the protocol to fail, and the taker's funds will be locked in a timelocked contract.

### Trigger a Failed Swap

Initiate a new swap, and while the taker is waiting for the funding transaction to be confirmed, stop one of the makers by terminating its process (e.g., using `CTRL+C` in the terminal where `makerd` is running).

```bash
taker coinswap
```

Output:

```bash
...
2025-09-10T22:00:45.908066166+05:30 INFO coinswap::taker::api - Funding tx Seen in Mempool. Waiting for confirmation for 30 secs
2025-09-10T22:00:46.747387294+05:30 ERROR coinswap::taker::api - error sending wait-notif to maker 6upcl2ga6icymftil4u62bvx7e7ydljsuysazfvnkpt2yobnpm3ushid.onion:6102 | IO(Custom { kind: Other, error: "general SOCKS server failure" })
2025-09-10T22:00:47.797797194+05:30 ERROR coinswap::taker::api - error sending wait-notif to maker reale54c4zdorlpjljtpfrv6tin7z26qcdwndn5mfn6l7t7k3l3bn5id.onion:6104 | IO(Custom { kind: Other, error: "general SOCKS server failure" })
2025-09-10T22:01:17.806521771+05:30 INFO coinswap::taker::api - Tx 8bd64f6ef55abd90ec23230d23491a9495a4dba5ef9822b6a53012006f225317 | Confirmed at 1
2025-09-10T22:01:17.808207071+05:30 INFO coinswap::wallet::rpc - Initializing wallet sync and save
2025-09-10T22:01:18.760803371+05:30 INFO coinswap::wallet::rpc - Completed wallet sync and save
2025-09-10T22:01:19.520647771+05:30 WARN coinswap::taker::api - Banning Maker : 6upcl2ga6icymftil4u62bvx7e7ydljsuysazfvnkpt2yobnpm3ushid.onion:6102
2025-09-10T22:01:19.520828271+05:30 ERROR coinswap::taker::api - Incoming SwapCoin Generation failed : IO(Custom { kind: Other, error: "general SOCKS server failure" })
2025-09-10T22:01:19.520860271+05:30 WARN coinswap::taker::api - Starting recovery from existing swap
...
2025-09-10T22:01:22.495612868+05:30 INFO coinswap::taker::api - Contract Tx : 90c664878791227b042838b9909832b04e6a50cdd559b120418a51a08263e850, reached confirmation : None, required : 60
2025-09-10T22:01:22.495757568+05:30 INFO coinswap::taker::api - 1 outgoing contracts detected | 0 timelock txs broadcasted.
```

### List Locked Funds

If the swap failed, this command will list the funds that are locked in a timelocked contract.

```bash
taker list-utxo-contract
{
  "addr": "bcrt1qkcz7yx64a4ta0rfgazkd0erqyuj6njnzmrfdewkszxg8gr229m2qpyyf8j",
  "amount": 19700,
  "confirmations": 0,
  "utxo_type": "timelock-contract"
}
```

### Recover the Funds

Use the `taker recover` command to reclaim the funds from the failed swap. This will create and broadcast a transaction to spend the timelocked UTXO after the timelock has expired.

```bash
taker recover
```

Output during recovery:

```bash
...
2025-09-10T22:20:25.413471527+05:30 INFO coinswap::taker::api - Contract Tx : 90c664878791227b042838b9909832b04e6a50cdd559b120418a51a08263e850, reached confirmation : Some(68), required : 60
2025-09-10T22:20:25.413500427+05:30 INFO coinswap::taker::api - Timelock maturity of 60 blocks for Contract Tx is reached : 90c664878791227b042838b9909832b04e6a50cdd559b120418a51a08263e850
2025-09-10T22:20:25.413507627+05:30 INFO coinswap::taker::api - Broadcasting timelocked tx: 5574ff04651236e13b18c15907517f59037752758ad3ab5421a3870a77f8109f
2025-09-10T22:20:25.443093327+05:30 INFO coinswap::taker::api - Removed Outgoing Swapcoin from Wallet, Contract Txid: 90c664878791227b042838b9909832b04e6a50cdd559b120418a51a08263e850
2025-09-10T22:20:25.443226827+05:30 INFO coinswap::wallet::rpc - Initializing wallet sync and save
2025-09-10T22:20:26.525635127+05:30 INFO coinswap::wallet::rpc - Completed wallet sync and save
2025-09-10T22:20:26.525776727+05:30 INFO coinswap::taker::api - 1 outgoing contracts detected | 1 timelock txs broadcasted.
2025-09-10T22:20:26.525805727+05:30 INFO coinswap::taker::api - All outgoing contracts redeemed. Cleared ongoing swap state
2025-09-10T22:20:26.525828527+05:30 INFO coinswap::taker::api - Recovery completed.
2025-09-10T22:20:26.525857427+05:30 INFO coinswap::taker::api - Shutting down taker.
2025-09-10T22:20:26.526175627+05:30 INFO coinswap::taker::api - offerbook data saved to disk.
2025-09-10T22:20:26.526518127+05:30 INFO coinswap::wallet::api - Wallet data saved to disk.
```

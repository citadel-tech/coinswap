# `maker-cli` Demo

`maker-cli` is a straightforward command-line tool designed as an RPC client for `makerd`. It allows you to connect to the server, retrieve vital information, and manage various server operations efficiently.

---

##  1. Create Maker Wallet and Sync

Use the following command to create a new wallet named `maker`. This generates a BIP84 descriptor wallet for the Signet test network.

```bash
maker-cli wallet --name maker
```

<details>
<summary> Sample Output</summary>

```json
{
  "name": "maker",
  "fingerprint": "bd59ecaa",
  "network": "Signet",
  "descriptor": "wpkh([bd59ecaa/84'/1'/0']tprv8ZgxMBicQKsPdCJiZoqZj4cHXE4C6DWqHLqByfGqSPc9sDU6Ruv1qSwANm3qZmBzeoFBX6N65CLctfMj2czPMi7NsmwH9paxJXGqMwMjWbR/0/*)#q9ljnezn",
  "change_descriptor": "wpkh([bd59ecaa/84'/1'/0']tprv8ZgxMBicQKsPdCJiZoqZj4cHXE4C6DWqHLqByfGqSPc9sDU6Ruv1qSwANm3qZmBzeoFBX6N65CLctfMj2czPMi7NsmwH9paxJXGqMwMjWbR/1/*)#n82amf2n"
}
```

</details>

After wallet creation, sync it with the blockchain to see your current UTXOs and balances:

```bash
maker-cli sync
```

---

##  2. Fund the Maker Wallet (via Faucet)

Get a receiving address for your wallet:

```bash
maker-cli address
```

Copy the address and go to the [Signet Faucet](https://signetfaucet.com) to receive testnet BTC.

After receiving coins, check your balance:

```bash
maker-cli balance
```

<details>
<summary> Sample Output</summary>

```json
{
  "total": "0.00350000 BTC",
  "trusted_pending": "0.00000000 BTC",
  "untrusted_pending": "0.00000000 BTC",
  "spendable": "0.00350000 BTC",
  "immature": "0.00000000 BTC"
}
```

</details>

---

##  3. Create a New Offer

Use the `create-offer` command to initiate a new coinswap offer. You specify how much BTC you're offering (maker amount), how much you expect from the taker (taker amount), and a lock time (in blocks) for the contract.

```bash
maker-cli create-offer \
  --maker-amount 50000 \
  --taker-amount 50000 \
  --lock-time 500
```

<details>
<summary> Sample Output</summary>

```json
{
  "offer_id": "48b42aef-46aa-46ae-afe0-02302d5f52d1"
}
```

</details>

This command will create and lock funds into a new offer contract.

---

##  4. Check the Status of Offers

To see all active offers and their current state:

```bash
maker-cli status
```

<details>
<summary> Sample Output</summary>

```json
[
  {
    "offer_id": "48b42aef-46aa-46ae-afe0-02302d5f52d1",
    "status": "OfferCreated"
  }
]
```

</details>

---

##  5. View Specific Offer Details

To get more info about a particular offer, run:

```bash
maker-cli offer --offer-id 48b42aef-46aa-46ae-afe0-02302d5f52d1
```

<details>
<summary> Sample Output</summary>

```json
{
  "offer_id": "48b42aef-46aa-46ae-afe0-02302d5f52d1",
  "maker_amount": 50000,
  "taker_amount": 50000,
  "lock_time": 500,
  "status": "OfferCreated"
}
```

</details>

---

##  6. Check Updated Balance After Offer Lock

Since the maker funds are locked in the offer, your spendable balance will reduce:

```bash
maker-cli balance
```

<details>
<summary> Sample Output</summary>

```json
{
  "total": "0.00300000 BTC",
  "trusted_pending": "0.00000000 BTC",
  "untrusted_pending": "0.00000000 BTC",
  "spendable": "0.00300000 BTC",
  "immature": "0.00000000 BTC"
}
```

</details>

---

##  Summary

| Step                | Command                         |
|---------------------|----------------------------------|
| Create Wallet       | `maker-cli wallet --name maker` |
| Sync Blockchain     | `maker-cli sync`                |
| Get Address         | `maker-cli address`             |
| Check Balance       | `maker-cli balance`             |
| Create Offer        | `maker-cli create-offer`        |
| Check Offer Status  | `maker-cli status`              |
| View Offer Details  | `maker-cli offer --offer-id`    |

---
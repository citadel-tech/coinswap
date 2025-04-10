#  `maker-cli` Demo

###  Create Maker Wallet and Sync

```bash
maker-cli wallet --name maker
```

<details>
<summary> Output</summary>

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

```bash
maker-cli sync
```

---

###  Fund Maker Wallet via Faucet

```bash
maker-cli address
```

Then visit: [https://signetfaucet.com](https://signetfaucet.com) and paste the generated address to get some Signet coins.

```bash
maker-cli balance
```

<details>
<summary> Output</summary>

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

###  Create a New Offer (Maker Contract)

```bash
maker-cli create-offer \
  --maker-amount 50000 \
  --taker-amount 50000 \
  --lock-time 500
```

<details>
<summary> Output</summary>

```json
{
  "offer_id": "48b42aef-46aa-46ae-afe0-02302d5f52d1"
}
```

</details>

---

###  Check Status of All Offers

```bash
maker-cli status
```

<details>
<summary> Output</summary>

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

###  View Offer Details

```bash
maker-cli offer --offer-id 48b42aef-46aa-46ae-afe0-02302d5f52d1
```

<details>
<summary> Output</summary>

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

###  Balance After Funding the Offer

```bash
maker-cli balance
```

<details>
<summary> Output</summary>

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
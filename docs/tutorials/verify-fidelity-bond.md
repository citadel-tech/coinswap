# How to Check Fidelity Bond on Blockchain

### Maker Fidelity Bond

To view all fidelity bonds (current and previous), use the following command:

```bash
maker-cli -p 127.0.0.1:6103 show-fidelity
```

Output:

```bash
[
  {
    "amount": 50000,
    "bond_value": 890,
    "index": 0,
    "outpoint": "1e32a14ff27ac4b036e691c033cd01bf619b87e38c828aa9e2b45c86bc8fdac3:0",
    "status": "Live"
  }
]
```

The `outpoint` field (e.g., `1e32a14ff27ac4b036e691c033cd01bf619b87e38c828aa9e2b45c86bc8fdac3:0`) represents the transaction output in the Bitcoin blockchain. To verify the fidelity bond:

1. **Using a Block Explorer**: Access a [Custom Signet Explorer](http://xlrj7ilheypw67premos73gxlcl7ha77kbhrqys7mydp7jve25olsxyd.onion/) (via Tor) and enter the transaction ID (TXID) to view details.
2. **Using `bitcoin-cli`**: Run the following command:

```bash
bitcoin-cli getrawtransaction <fidelity_bond_txid> true
```

Replace `<fidelity_bond_txid>` with the actual transaction ID. This command provides detailed transaction information, including confirmation status and locked amounts.

### Taker Fidelity Bond

The taker automatically verifies the maker's fidelity bond when fetching offers. This ensures the maker has a valid and active fidelity bond before proceeding with a swap.

To manually verify the fidelity bond:

1. Check the offer data stored in `.coinswap/taker/offerbook.json` after fetching offers.
2. Use a [Custom Signet Explorer](http://xlrj7ilheypw67premos73gxlcl7ha77kbhrqys7mydp7jve25olsxyd.onion/) or `bitcoin-cli` (as described above) to verify the bond's transaction ID.

# Taproot Coinswap Protocol Documentation

## Overview

This document describes the complete taproot-based coinswap protocol implementation using MuSig2 signatures for enhanced privacy and efficiency. The protocol enables trustless atomic swaps between a taker and multiple makers in a cyclic flow.

## Protocol Architecture

### Core Concepts

- **Cyclic Flow**: Funds flow in a circle (Taker → Maker0 → Maker1 → ... → Taker) to break transaction links
- **Taproot Contracts**: P2TR outputs with script trees for cooperative (key path) and non-cooperative (script path) spending
- **MuSig2 Signatures**: Schnorr signature aggregation for the cooperative spending path
- **Dual Spending Modes**: 
  - **Happy Path**: MuSig2 key spend (cooperative, private, efficient)
  - **Recovery Path**: Script spend using hashlock/timelock (non-cooperative, for failures)

### Transaction Structure

Each contract transaction uses P2TR with this structure:
```
P2TR Output:
├── Internal Key: MuSig2_KeyAgg(party1_pubkey, party2_pubkey)
├── Script Tree:
│   ├── Hashlock Script: "OP_SHA256 <hash> OP_EQUALVERIFY <receiver_pubkey> OP_CHECKSIG"
│   └── Timelock Script: "<locktime> OP_CLTV OP_DROP <sender_pubkey> OP_CHECKSIG"
└── Tap Tweak: Derived from script tree merkle root
```

## Complete Message Flow (1 Taker + 2 Makers)

### Phase 1: Discovery and Negotiation

```
1. Taker → Maker0: GetOffer
2. Maker0 → Taker: RespOffer { max_size, min_size, fee_rate, tweakable_point }

3. Taker → Maker1: GetOffer  
4. Maker1 → Taker: RespOffer { max_size, min_size, fee_rate, tweakable_point }

5. Taker → Maker0: SwapDetails { amount, maker_count, timelock }
6. Maker0 → Taker: AckResponse::Ack

7. Taker → Maker1: SwapDetails { amount, maker_count, timelock }
8. Maker1 → Taker: AckResponse::Ack
```

### Phase 2: Contract Creation (Cyclic Flow)

#### Step 1: Taker → Maker0 Contract
```
9. Taker creates and broadcasts contract transaction:
   - Input: Taker's UTXO
   - Output: P2TR(MuSig2(taker_pubkey, maker0_pubkey), script_tree)
   - Amount: swap_amount

10. Taker → Maker0: SendersContract {
    contract_txs: [taker_to_maker0_txid],
    pubkeys_a: [taker_pubkey],
    next_party_tweakable_point: maker1_pubkey,
    next_party_pub_nonces: [taker_contract_nonce],
}

11. Maker0 → Taker: ReceiversContract {
    contract_txs: [maker0_to_maker1_txid],
    pubkeys_b: [maker0_pubkey], 
    receiver_nonces: [maker0_contract_nonce],
}
```

#### Step 2: Maker0 → Maker1 Contract (Forwarded via Taker)
```
12. Taker → Maker1: SendersContract {
    contract_txs: [maker0_to_maker1_txid],     // Forwarded from Maker0
    pubkeys_a: [maker0_pubkey],                // Forwarded from Maker0
    next_party_tweakable_point: taker_pubkey,  // Cycle back to taker
    next_party_pub_nonces: [maker0_contract_nonce],
}

13. Maker1 → Taker: ReceiversContract {
    contract_txs: [maker1_to_taker_txid],      // Final contract back to taker
    pubkeys_b: [maker1_pubkey],
    receiver_nonces: [maker1_contract_nonce],
}
```

### Phase 3: Sequential Sweeping (Revised)

**Design Principle**: After contract creation, the taker initiates backwards sweeping starting with the last maker and working backwards through the chain. Each receiving party constructs their spending transaction.

#### Flow Description (1 Taker + 2 Makers: Maker0 → Maker1 → Taker)

```
13. Taker → Maker1 (Last Maker): SpendingTxAndReceiverNonce {
    spending_transaction: taker_constructed_spending_tx_for_maker1_to_taker_contract,
    receiver_nonce: taker_nonce_for_maker1_to_taker_contract
}

14. Maker1 → Taker: NoncesPartialSigsAndSpendingTx {
    sender_nonce: maker1_nonce_for_maker1_to_taker_contract,
    receiver_nonce: maker1_nonce_for_maker0_to_maker1_contract,
    partial_signatures: [maker1_partial_sig_for_maker1_to_taker_contract],
    spending_transaction: maker1_constructed_spending_tx_for_maker0_to_maker1_contract
}

15. Taker completes and broadcasts taker's spending transaction:
    - Generates taker_partial_sig_for_maker1_to_taker_contract
    - Aggregates with maker1's partial signature
    - Broadcasts spending transaction to claim from maker1_to_taker_contract

16. Taker → Maker0: SpendingTxAndReceiverNonce {
    spending_transaction: maker1_constructed_spending_tx_for_maker0_to_maker1_contract,  // From step 14
    receiver_nonce: maker1_nonce_for_maker0_to_maker1_contract  // From step 14
}

17. Maker0 → Taker: NoncesPartialSigsAndSpendingTx {
    sender_nonce: maker0_nonce_for_maker0_to_maker1_contract,
    receiver_nonce: maker0_nonce_for_taker_to_maker0_contract,
    partial_signatures: [maker0_partial_sig_for_maker0_to_maker1_contract],
    spending_transaction: maker0_constructed_spending_tx_for_taker_to_maker0_contract
}

18. Taker → Maker1: PartialSigAndSendersNonce {
    partial_signatures: [maker0_partial_sig_for_maker0_to_maker1_contract],  // From step 17
    sender_nonce: maker0_nonce_for_maker0_to_maker1_contract  // From step 17
}

19. Maker1 completes and broadcasts maker1's spending transaction:
    - Generates maker1_partial_sig_for_maker0_to_maker1_contract
    - Aggregates with maker0's partial signature
    - Broadcasts spending transaction to claim from maker0_to_maker1_contract

20. Taker → Maker0: PartialSigAndSendersNonce {
    partial_signatures: [taker_partial_sig_for_taker_to_maker0_contract],
    sender_nonce: taker_nonce_for_taker_to_maker0_contract
}

21. Maker0 completes and broadcasts maker0's spending transaction:
    - Generates maker0_partial_sig_for_taker_to_maker0_contract
    - Aggregates with taker's partial signature
    - Broadcasts spending transaction to claim from taker_to_maker0_contract
```

#### Message Types for New Protocol

```rust
SpendingTxAndReceiverNonce {
    spending_transaction: Transaction,
    receiver_nonce: SerializablePublicNonce,
}

NoncesPartialSigsAndSpendingTx {
    sender_nonce: SerializablePublicNonce,
    receiver_nonce: SerializablePublicNonce,
    partial_signatures: Vec<SerializablePartialSignature>,
    spending_transaction: Transaction,
}

PartialSigAndSendersNonce {
    partial_signatures: Vec<SerializablePartialSignature>,
    sender_nonce: SerializablePublicNonce,
}
```

#### Key Characteristics

1. **Backwards Flow**: Taker starts with last maker and works backwards
2. **Receiver Constructs**: Each party constructs the spending transaction for the contract they're claiming from
3. **Atomic Coordination**: Taker coordinates all signature exchanges but each party broadcasts their own transaction
4. **Sequential Execution**: Taker sweeps first, then each maker sweeps in reverse order

## Spending Transaction Details

### All Three Spending Transactions

#### Maker0's Spending Transaction (from Taker's Contract)
```rust
Maker0_Spending_Transaction:
├── Input[0]:
│   ├── previous_output: taker_to_maker0_txid:0
│   ├── script_sig: empty
│   └── witness: [maker0_taker_aggregated_signature]
└── Output[0]:
    ├── value: swap_amount - fees
    └── script_pubkey: maker0_receiving_address
```

#### Maker1's Spending Transaction (from Maker0's Contract)
```rust
Maker1_Spending_Transaction:
├── Input[0]:
│   ├── previous_output: maker0_to_maker1_txid:0
│   ├── script_sig: empty
│   └── witness: [maker1_maker0_aggregated_signature]
└── Output[0]:
    ├── value: swap_amount - fees
    └── script_pubkey: maker1_receiving_address
```

#### Taker's Spending Transaction (from Maker1's Contract)
```rust
Taker_Spending_Transaction:
├── Input[0]:
│   ├── previous_output: maker1_to_taker_txid:0
│   ├── script_sig: empty
│   └── witness: [taker_maker1_aggregated_signature]
└── Output[0]:
    ├── value: swap_amount - fees
    └── script_pubkey: taker_receiving_address
```

### Sighash Calculation
Each pair of parties must calculate identical sighash for their respective spending transaction:

```rust
// Example: Taker and Maker1 calculating sighash for taker's spending tx
let sighash = SighashCache::new(&taker_spending_tx)
    .taproot_key_spend_signature_hash(
        0,                           // input_index
        &prevouts,                   // Previous outputs (maker1's contract output)
        TapSighashType::Default      // sighash_type
    )?;
let message = Message::from(sighash);

// Both taker and maker1 use this same message for their partial signatures
```

## Complete Protocol Summary

### Total Message Flow
The complete taproot coinswap involves **28 messages** across 3 phases:

1. **Discovery (8 messages)**: Offer fetching and swap negotiation
2. **Contract Creation (4 messages)**: Cyclic contract setup
3. **Sequential Cooperative Spending (16 messages)**: Taker-coordinated MuSig2 signature exchanges for all three spending transactions

### Execution Order
The protocol phases execute sequentially with taker coordination:

```
Phase 1: Discovery & Negotiation (messages 1-8)
    ↓
Phase 2: Contract Creation (messages 9-12)
    ↓
Phase 3A: Maker0 spends from Taker's contract (messages 13-17, taker-initiated)
    ↓
Phase 3B: Maker1 spends from Maker0's contract (messages 18-24, via Taker relay)  
    ↓
Phase 3C: Taker spends from Maker1's contract (messages 25-28)
```

### Non-Cooperative Cases (Recovery Paths)

#### Hashlock Path (Receiver Claiming)
```rust
// Taker can claim using preimage without maker cooperation
witness: [
    taker_signature,
    preimage,
    hashlock_script,
    control_block,  // Proves script is in taproot tree
]
```

#### Timelock Path (Sender Recovery)
```rust
// Maker can recover funds after timeout without receiver cooperation
witness: [
    maker_signature,
    empty_vector,   // No preimage needed
    timelock_script,
    control_block,
]
```

## Message Types

### Discovery Messages
```rust
GetOffer { }
RespOffer { 
    max_size: Amount,
    min_size: Amount, 
    fee_rate: f64,
    tweakable_point: PublicKey,
}

SwapDetails {
    amount: Amount,
    maker_count: u8,
    timelock: u16,
}
AckResponse::Ack | AckResponse::Nack
```

### Contract Messages
```rust
SendersContract {
    contract_txs: Vec<Txid>,
    pubkeys_a: Vec<PublicKey>,
    next_party_tweakable_point: PublicKey,
    next_party_pub_nonces: Vec<SerializablePublicNonce>,
}

ReceiversContract {
    contract_txs: Vec<Txid>,
    pubkeys_b: Vec<PublicKey>,
    receiver_nonces: Vec<SerializablePublicNonce>,
}
```

### Spending Messages
```rust
SenderNonce // Initiates sweep for a maker

ReceiverNonce {
    sender_nonces: Vec<SerializablePublicNonce>,
    partial_signatures: Vec<SerializablePartialSignature>,  // Usually empty for nonce exchange
}

PartialSignature {
    partial_signatures: Vec<SerializablePartialSignature>,
    sender_nonces: Vec<SerializablePublicNonce>,
}
```

## Implementation Architecture

### Key Components

1. **MuSig2 Engine** (`src/protocol/musig2.rs`)
   - Nonce generation and aggregation
   - Partial signature creation and aggregation
   - Key aggregation for internal keys

2. **Taproot Contracts** (`src/protocol/contract2.rs`)
   - Script tree construction
   - P2TR output creation
   - Control block generation

3. **Protocol Messages** (`src/protocol/messages2.rs`)
   - Serializable message types
   - Network communication protocol

4. **State Management**
   - Taker: `OngoingSwapState` for tracking multi-maker flow
   - Maker: `ConnectionState` persisted across TCP connections
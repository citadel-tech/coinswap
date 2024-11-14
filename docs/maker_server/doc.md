# CoinSwap Protocol Message Flow Documentation

## Overview
This document details the complete message flow between Taker, Maker, and Directory servers in the CoinSwap protocol.

## Server Components
1. Directory Server (DNS)
2. Maker Servers (Multiple instances)
3. Taker Client

## Protocol Flow

### 1. Initial Setup

#### Maker Server Initialization
```rust
// Maker server configuration
struct MakerConfig {
    listen_addr: SocketAddr,
    directory_servers: Vec<String>,
    fidelity_amount: Amount,
    min_swap_amount: Amount,
    max_swap_amount: Amount,
}
```

#### Fidelity Bond Creation
From test implementation:

```67:79:tests/abort2_case1.rs
    // Coins for fidelity creation
    makers.iter().for_each(|maker| {
        let maker_addrs = maker
            .get_wallet()
            .write()
            .unwrap()
            .get_next_external_address()
            .unwrap();
        test_framework.send_to_address(&maker_addrs, Amount::from_btc(0.05).unwrap());
    });

    // confirm balances
    test_framework.generate_blocks(1);
```


### 2. Message Flow Sequence

#### A. Directory Server Registration
```plaintext
MAKER -> DNS: POST /register
{
    "address": "127.0.0.1:6102",
    "fidelity_proof": {...},
    "features": ["v1"]
}

DNS -> MAKER: 200 OK
```

#### B. Taker-Maker Initial Connection
```plaintext
14:16:33.712653 INFO [16102] <=== TakerHello
14:16:33.715251 INFO [16102] ===> MakerHello
14:16:33.722437 INFO [16102] <=== ReqGiveOffer
14:16:33.724105 INFO [16102] ===> RespOffer
```

#### C. Contract Setup Phase
```plaintext
14:16:36.720270 INFO [16102] <=== ReqContractSigsForSender
14:16:36.725411 INFO [16102] Total Funding Amount = 0.00500000 BTC
14:16:36.725470 INFO [16102] ===> RespContractSigsForSender
```

#### D. Hash Preimage Exchange
```plaintext
14:40:09.736389 INFO [16102] <=== RespHashPreimage
14:40:09.737329 INFO [16102] received preimage for hashvalue=8d3acc8a2b9af5a8db7c818e93cdcf8f5aed5686
14:40:09.737329 INFO [16102] ===> RespPrivKeyHandover
```

### 3. Message Structures

#### TakerHello
```rust
struct TakerHello {
    version: u32,
    nonce: [u8; 32],
    features: Vec<Feature>,
}
```

#### MakerHello
```rust
struct MakerHello {
    version: u32,
    fidelity_bond: FidelityBond,
    supported_features: Vec<Feature>,
}
```

#### Contract Messages
```rust
struct ReqContractSigsForSender {
    funding_txids: Vec<Txid>,
    amounts: Vec<Amount>,
    contract_params: ContractParams,
}

struct RespContractSigsForSender {
    signatures: Vec<Signature>,
    error: Option<String>,
}
```

### 4. Error Handling

The logs show several timeout and retry mechanisms:

```plaintext
14:40:09.744596 INFO [6102] No response from 127.0.0.1 in 8.973170375s
14:40:09.992809 INFO [16102] No response from 127.0.0.1 in 18.003951708s
```

Error recovery sequence:
1. Connection timeout detection
2. Retry attempt with backoff
3. State recovery if needed
4. Connection re-establishment

### 5. Swap Parameters

Standard configuration used in tests:

```131:137:tests/malice1.rs
    let swap_params = SwapParams {
        send_amount: Amount::from_sat(500000),
        maker_count: 2,
        tx_count: 3,
        required_confirms: 1,
        fee_rate: Amount::from_sat(1000),
    };
```


### 6. Thread Management

Server threads are managed with proper synchronization:

```97:104:tests/abort2_case3.rs
    makers.iter().for_each(|maker| {
        while !*maker.is_setup_complete.read().unwrap() {
            log::info!("Waiting for maker setup completion");
            // Introduce a delay of 10 seconds to prevent write lock starvation.
            thread::sleep(Duration::from_secs(10));
            continue;
        }
    });
```


## Security Considerations

1. **Fidelity Bond Verification**
- Amount: 0.05 BTC
- Confirmation requirement: 1 block
- Timelock period: 144 blocks

2. **Transaction Safety**
```plaintext
14:16:36.725411 INFO Funding Amount = 0.00500000 BTC
14:16:36.725470 INFO Contract Signatures Verified
```

3. **Balance Verification**

```77:102:tests/malice1.rs
    let org_taker_balance_fidelity = taker
        .read()
        .unwrap()
        .get_wallet()
        .balance_fidelity_bonds(Some(&all_utxos))
        .unwrap();
    let org_taker_balance_descriptor_utxo = taker
        .read()
        .unwrap()
        .get_wallet()
        .balance_descriptor_utxo(Some(&all_utxos))
        .unwrap();
    let org_taker_balance_swap_coins = taker
        .read()
        .unwrap()
        .get_wallet()
        .balance_swap_coins(Some(&all_utxos))
        .unwrap();
    let org_taker_balance_live_contract = taker
        .read()
        .unwrap()
        .get_wallet()
        .balance_live_contract(Some(&all_utxos))
        .unwrap();

    let org_taker_balance = org_taker_balance_descriptor_utxo + org_taker_balance_swap_coins;
```


## Directory Server Protocol

### Registration Flow
1. Maker startup
2. Directory server connection
3. Registration message
4. Periodic heartbeat

### Heartbeat Message
```plaintext
INFO [16102] Directory server address: 127.0.0.1:8080
INFO [16102] Successfully sent maker address to directory
```

## Shutdown Sequence

```plaintext
14:40:30.691642 INFO [16102] Closing Thread: Contract Watcher Thread
14:40:30.691749 INFO [16102] Thread closing result: Ok(())
14:40:30.692020 INFO [16102] Closing Thread: Client Handler Thread
14:40:30.692829 INFO Shutdown wallet sync initiated.
14:40:31.067002 INFO Shutdown wallet syncing completed.
```

This documentation provides a comprehensive overview of the message flow and protocol implementation in the CoinSwap system. For detailed protocol specifications, refer to the original design document([1](https://gist.github.com/chris-belcher/ca5051285c6f8d38693fd127575be44d)).
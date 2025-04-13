## What Is Coinswap?

CoinSwap is a **decentralized atomic swap protocol** that allows two or more Bitcoin users to **exchange ownership of UTXOs** without leaving a trace on the blockchain. It uses **Hashed Time-Locked Contracts (HTLCs)** and runs over **Tor** to ensure **privacy, trust minimization, and censorship resistance**.

It differs from centralized exchanges or custodial mixers by enabling peer-to-peer swaps directly between participants. CoinSwap facilitates:

- **Off-chain UTXO ownership changes** that appear like regular Bitcoin transactions on-chain
- **Composable multi-hop swaps** such as `Alice → Bob → Carol → Alice` to obscure paths
- **Trustless coordination** using smart contracts and time-locked conditions

### Key Features

- **Smart-client, dumb-server architecture**: The client (Taker) coordinates the swap logic and routes, while the server (Maker) acts as a passive liquidity provider.
- **Incentivized Marketplace**: Makers register by locking **Fidelity Bonds** (UTXOs) which act as a stake. These give visibility in the offerbook and help resist Sybil attacks.
- **Privacy & Anonymity**: The protocol relies on **Tor** for all communications and leverages Bitcoin Script to provide **provable, anonymous, and verifiable swaps**.
- **Failure Recovery**: If any party misbehaves or drops out mid-swap, others can **safely reclaim funds** via timeout paths in the HTLCs.

### How it Works

1. **Taker queries the offerboard** (hosted via DNS or alternative discovery mechanism) for available Makers and their parameters.
2. **Swap is initiated** via Tor, where Taker and Maker construct a contract involving HTLCs and pre-signed refund paths.
3. **Bond verification** ensures the Maker has skin in the game.
4. **Transaction chain setup** occurs, followed by confirmation monitoring.
5. If successful, swap finalizes; if a party aborts, refund paths trigger after timeout.

### Protocol Highlights

- **Atomicity**: Either the whole swap completes or nothing does.
- **Confidentiality**: All interaction is encrypted over Tor and indistinguishable from normal UTXO spends on-chain.
- **Extensibility**: The protocol allows future upgrades like federated matchmaking, taproot enhancements, and batched swaps.

### References

- [Project README](../README.md) – high-level overview and architecture
- [Updated App Demos](../docs/demo.md) – walkthrough of UI/CLI usage
- [Protocol Specification](https://github.com/citadel-tech/Coinswap-Protocol-Specification) – formal contract design, message flows

---

## Session Timeline

| **Topic**             | **Duration (mins)** | **Format**      | **Host**  |
|------------------------|---------------------|-----------------|-----------|
| Intro to CoinSwap      | 15                  | Presentation    | Rishabh   |
| CoinSwap Live Demo     | 30                  | Workshop        | Raj       |
| Problem Statement      | 15                  | Presentation    | Raj       |
| Brainstorming Session  | 30                  | Discussion      | Raj       |

---

## Prerequisites

Before joining the workshop, **complete the following steps** to ensure full participation:

- **Read the [README](../README.md)** to understand the motivation, architecture, and security model
- **Follow the [Demo Guide](./demo.md)** for installing and configuring the CLI apps

### Environment Setup Checklist:

To act as a **Maker**, ensure:

- A synced `bitcoind` node on **Testnet4**
- Wallet with at least **501,000 sats**, split as:
  - `500,000 sats` for Fidelity Bond (locked UTXO)
  - `1,000 sats` for the bond transaction fee
  - `10,000 sats` minimum liquidity for swap offers

- Built binaries:
  - `coinswap-maker`
  - `coinswap-taker`

- A **Tor daemon** running with:
  - `SOCKSPort 9050`
  - `ControlPort 9051`
  - Authentication via password or cookie (see [Tor Setup](./tor.md))

---

## The Session

### **Introduction**

We'll start with the **background and motivation** behind CoinSwap:

- What problem it solves
- How it improves upon other privacy tools (e.g., CoinJoin, Lightning)
- Why on-chain anonymity matters
- Basic protocol flow with HTLCs

An **open Q&A** session will follow to clarify common doubts.

---

### **Live Demo**

We’ll simulate an actual swap session on Testnet:

- Participants take roles as **Makers** and **Takers**
- Set up and execute **multi-hop swaps**
- Observe logs for key events:
  - Offer discovery
  - Swap negotiation
  - HTLC creation
  - Timeouts and refund paths
- Simulate **fault scenarios** like Maker abort or invalid signatures
- Showcase **client-side recovery** using HTLC timeout logic

---

### **Problem Statement**

> Even though swaps are decentralized, **offer discovery is centralized** using a DNS server.

If the DNS server fails, the **entire marketplace is disrupted**, making it a **single point of failure**.

---

### **Brainstorming**

We’ll ideate on ways to **decentralize offer discovery**, including:

- **Tor-based service discovery**
- **Decentralized naming systems (e.g., ENS, Namecoin)**
- **Gossip protocols** for peer-to-peer broadcasting
- **Federated or relay-based offerboards**

Goal: find robust, censorship-resistant alternatives to the current DNS-based approach.

---

### **Learnings & Takeaways**

By the end of the workshop, participants will:

- Understand how CoinSwap achieves **on-chain privacy** using atomic swaps
- Learn to coordinate UTXO transfers via **HTLCs and off-chain negotiation**
- Know how **Fidelity Bonds** prevent Sybil attacks and help build trustless markets
- Be able to **run their own Maker or Taker** node and participate in the network
- Explore the challenges of building **decentralized coordination** tools

---

Feel free to ask questions on Discord, raise issues on GitHub, or explore the codebase to contribute!
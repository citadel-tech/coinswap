## What Is Coinswap?

Coinswap is a decentralized atomic swaps protocol designed to operate over a peer-to-peer network using Tor. The protocol provides a mechanism for peers to swap their UTXOs with other peers in the network, resulting in the transfer of ownership between the two UTXOs without leaving an on-chain footprint.

The protocol includes a peer-to-peer messaging system, similar to the Lightning Network, enabling peers to perform swaps without trusting each other by utilizing the well-established HTLC (Hashed Time-Locked Contract) script constructions. The protocol supports *composable swaps*, allowing for the creation of swap chains such as `Alice --> Bob --> Carol --> Alice`. At the end of the swap, Alice ends up with Carol's UTXO, Carol ends up with Bob's, and Bob ends up with Alice's. 

In this scenario, Alice acts as the client, while Bob and Carol act as service providers. The client is responsible for paying all the fees, which consist of two components: 
- swap transaction fees 
- service provider fees. 

For each swap, the service providers earn fees, incentivizing node operators to *lock liquidity* and *earn yield*. Unlike the Lightning Network, swap service software does not require active node management. It is a plug-and-play system, making it much easier to integrate swap services into existing node modules.

The service providers are invisible to each other and only relay messages via the client. The client acts as the relay and handles the majority of protocol validations, while the service providers act as simple daemons that respond to client messages. The protocol follows the `smart-client-dumb-server` design philosophy, making the servers lightweight and capable of running in constrained environments. 

At any point during a swap, if any party misbehaves, the other parties can recover their funds from the swap using the HTLC's time-lock path.

The protocol also includes a marketplace with dynamic offer data attached to a **Fidelity Bond** and a Tor address. The Fidelity Bond is a time-locked Bitcoin UTXO that service providers must display in the marketplace to be accepted for swaps. If a provider misbehaves, clients in the marketplace can punish them by refusing to swap with the same Fidelity Bond in the future. Fidelity Bonds thus serve as an identity mechanism, providing **provable costliness** to create Sybil resistance in the decentralized marketplace.

The protocol is in its early stages and has several open questions and potential vulnerabilities. However, it is a promising approach to decentralized atomic swaps, and we are excited to see where it goes.

For more details, please refer to the project [README](../README.md) and check out the [App Demos](./). 

A more detailed [protocol specification](https://github.com/citadel-tech/Coinswap-Protocol-Specification) is also available.

---

## Session Timeline

| **Topic**             | **Duration (mins)** | **Format**      | **Host**  |
|------------------------|---------------------|-----------------|-----------|
| Intro to Coinswap      | 15                  | Presentation    | Rishabh   |
| Coinswap Live Demo     | 30                  | Workshop        | Raj       |
| Problem Statement      | 15                  | Presentation    | Raj       |
| Brainstorming Session  | 30                  | Discussion      | Raj       |

---

## Prerequisites

To actively engage in the session, please prepare with the following:

- **Read up on Coinswap**: start with the project [README](../../README.md). 
- **Set up your environment**: Follow the [demo documentation](./demo.md) to set up your system. You will need:
  - A running `bitcoind` node on your local machine, synced on Testnet4.
  - At least 511,000 sats (500,000 sats for the Fidelity Bond + 1000 sats for fidelity tx fee + 10,000 sats as minimum swap liquidity) of balance in your wallet if you are running maker.
  - Instructions for setting up `bitcoind`, connecting the apps, and running the entire Coinswap process are provided in the demo documentation.

---

## The Session

### **Introduction**
We will begin with a basic introduction to Coinswap and lay the foundation for the session. Participants are encouraged to ask questions to clarify any doubts before moving to the next section.

---

### **Demo**
In this segment, we will set up a complete swap marketplace with multiple makers and takers on our systems. Participants will role-play as takers and makers, performing a multi-hop swap with each other. We will monitor the live system logs on our laptops to observe the progress of the swap in real-time.

If time permits, we will conduct another swap round with a malicious maker who drops out during the swap. This will demonstrate the recovery process and highlight any potential traces left on the blockchain.

---

### **Problem Statement**
The demo will reveal a centralization vector in the marketplace: the DNS server. Without the DNS, clients cannot discover servers, and taking the DNS offline would disrupt the entire marketplace.

---

### **Brainstorming**
We will explore the design space of decentralized DNS systems and gossip protocols to address this centralization issue. Participants will discuss the pros and cons of various designs, and through a collective brainstorming session, we will aim to propose a suitable design for Coinswap.

---

### **Learnings**
By the end of the session, participants can expect to gain a solid understanding of concepts such as atomic swaps, HTLCs, decentralized marketplaces, gossip protocols, and more.
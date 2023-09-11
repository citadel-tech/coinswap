# Developer resources

<!-- TOC -->
- [Developer resources](#developer-resources)
  - [What it is](#what-it-is)
  - [How CoinSwap works](#how-coinswap-works)
  - [Notes on architecture](#notes-on-architecture)
  - [Protocol between takers and makers](#protocol-between-takers-and-makers)
  - [Code Structure](#code-structure)
  - [Further reading](#further-reading)
<!-- /TOC -->

## What it is
The Coinswap protocol enhances privacy on the Bitcoin network, specifically addressing transaction ownership traceability through chain analysis.

Imagine Alice wants to send bitcoin with maximum privacy. She initiates a unique transaction that appears ordinary on the blockchain, with her coins seemingly moving from address A to B. However, the coins actually end up in an unrelated address Z. This process confounds any attempt to trace ownership.

This protocol also benefits users like Carol, who use regular wallets. Her transactions appear the same as Alice's, adding uncertainty to any analysis. Even users unaware of this software enjoy improved privacy.

In a world where privacy is vital due to data collection by advertisers and institutions, this enhancement is significant. Moreover, it bolsters Bitcoin's fungibility, making it a more effective form of currency.

## How CoinSwap works

In a two-party coinswap, Alice and Bob can swap a coin in a non-custodial way, where neither party can steal from each other. At worst, they can waste time and miner fees.

To start a coinswap, Alice will obtain one of Bob's public keys and use that to create a 2-of-2 multisignature address (known as Alice's coinswap address) made from Alice's and Bob's public keys. Alice will create a transaction (known as Alice's funding transaction) sending some of her coins (known as the coinswap amount) into this 2-of-2 multisig, but before she actually broadcasts this transaction she will ask Bob to use his corresponding private key to sign a transaction (known as Alice contract transaction) which sends the coins back to Alice after a timeout. Even though Alice's coins would be in a 2-of-2 multisig not controlled by her, she knows that if she broadcasts her contract transaction she will be able to get her coins back even if Bob disappears.

Soon after all this has happened, Bob will do a similar thing but mirrored. Bob will obtain one of Alice's public keys and from it Bob's coinswap address. Bob creates a funding transaction paying to it the same coinswap amount, but before he broadcasts it he gets Alice to sign a contract transaction which sends Bob's coins back to him after a timeout.

At this point both Alice and Bob are able to broadcast their funding transactions paying coins into multisig addresses, and if they want they can get those coins back by broadcasting their contract transactions and waiting for the timeout. The trick with coinswap is that the contract transaction script contains a second clause: it is also possible for the other party to get the coins by providing a hash preimage (e.g. HX = sha256(X)) without waiting for a timeout. The effect of this is that if the hash preimage is revealed to both parties then the coins in the multisig addresses have transferred possession off-chain to the other party who originally didn't own those coins.

When the preimage is not known, Alice can use her contract transaction to get coins from Alice's multisig address after a timeout, and Bob can use his contract transaction to get coins from the Bob multisig address after a timeout. After the preimage is known, Alice can use Bob's contract transaction and the preimage to get coins from Bob's multisig address, and also Bob can use Alice's contract transaction and the preimage to get the coins from Alice's multisig address.

Here is a diagram of Alice and Bob's coins and how they swap possession after a coinswap:
```
                                              Alice after a timeout
                                             /
                                            /
Alice's coins ------> Alice coinswap address
                                            \
                                             \
                                              Bob with knowledge of the hash preimage


                                          Bob after a timeout
                                         /
                                        /
Bob's coins ------> Bob coinswap address
                                        \
                                         \
                                          Alice with knowledge of the hash preimage
```

If Alice attempts to take the coins from Bob's coinswap address using her knowledge of the hash preimage and Bob's contract transaction, then Bob will be able to read the value of the hash preimage from the blockchain, and use it to take the coins from Alice's coinswap address. This happens in the worst case, but in virtually all real-life situations it will never get to that point. The contracts usually always stay unbroadcasted.

So at this point we've reached a situation where if Alice gets paid then Bob cannot fail to get paid, and vice versa. Now to save time and miner fees, the party which started with knowledge of the hash preimage will reveal it, and both parties will send each other their private keys corresponding to their public keys in the 2-of-2 multisigs. After this private key handover Alice will know both private keys in the relevant multisig address, and so those coins are in her sole possession. The same is true for Bob.

```
Alice's coins ----> Bob's address

Bob's coins ----> Alice's address
```

In a successful coinswap, Alice's and Bob's coinswap addresses transform off-chain to be possessed by the other party


[Bitcoin's script](https://en.bitcoin.it/wiki/Script) is used to code these timelock and hashlock conditions. Diagrams of the transactions:
```
= Alice's funding transaction =
Alice's inputs -----> multisig (Alice pubkey + Bob pubkey)

= Bob's funding transaction =
Bob's inputs -----> multisig (Bob pubkey + Alice pubkey)

= Alice's contract transaction=
multisig (Alice pubkey + Bob pubkey) -----> contract script (Alice pubkey + timelock OR Bob pubkey + hashlock)

= Bob's contract transaction=
multisig (Bob pubkey + Alice pubkey) -----> contract script (Bob pubkey + timelock OR Alice pubkey + hashlock)
```

The contract transactions are only ever used if a dispute occurs. If all goes well the contract transactions never hit the blockchain and so the hashlock is never revealed, and therefore the coinswap improves privacy by delinking the transaction graph.

The party which starts with knowledge of the hash preimage must have a longer timeout, this means there is always enough time for the party without knowledge of the preimage to read the preimage from the blockchain and get their own transaction confirmed.

This explanation describes the simplest form of coinswap. On its own it isn't enough to build a really great private system. For more building blocks read the [design document of this project](https://gist.github.com/chris-belcher/9144bd57a91c194e332fb5ca371d0964).

## Notes on architecture

Makers are servers which run Tor hidden services (or possibly other hosting solutions in case Tor ever stops working). Takers connect to them. Makers never connect to each other.

Diagram of connections for a 4-hop coinswap:
```
        ---- Bob
       /
      /
Alice ------ Charlie
      \
       \
        ---- Dennis
```

The coinswap itself is multi-hop:

```
Alice ===> Bob ===> Charlie ===> Dennis ===> Alice
```

Makers are not even meant to know how many other makers there are in the route. They just offer their services, offer their fees, protect themselves from DOS, complete the coinswaps and make sure they get paid those fees. We aim to have makers have as little state as possible, which should help with DOS-resistance.

All the big decisions are made by takers (which makes sense because takers are paying, and the customer is always right.)
Decisions like:
* How many makers in the route
* How many transactions in the multi-transaction coinswap
* How long to wait between funding txes
* The bitcoin amount in the coinswap

In this protocol it's always important to as much as possible avoid DOS attack opportunities, especially against makers.


## Protocol between takers and makers

Alice is the taker, Bob, Charlie and Dennis are makers. For a detailed explanation including definitions see the mailing list email [here](https://lists.linuxfoundation.org/pipermail/bitcoin-dev/2020-October/018221.html). That email should be read first and then you can jump back to the diagram below when needed while reading the code.

Protocol messages are defined by the structs found in `src/messages.rs` and serialized into json with rust's serde crate.

```
 | Alice           | Bob             | Charlie         |  Dennis         | message, or (step) if repeat
 |=================|=================|=================|=================|
0. AB/A htlc     ---->               |                 |                 | sign senders contract
1.               <---- AB/A htlc B/2 |                 |                 | senders contract sig
2.    ************** BROADCAST AND MINE ALICE FUNDING TX *************** |
3.    A fund     ---->               |                 |                 | proof of funding
4.               <----AB/B+BC/B htlc |                 |                 | sign senders and receivers contract
5. BC/B htlc     ---------------------->               |                 | (0)
6.               <---------------------- BC/B htlc C/2 |                 | (1)
7. AB/B+BC/B A+C/2--->               |                 |                 | senders and receivers contract sig
8.    ************** BROADCAST AND MINE BOB FUNDING TX ***************   |
A.    B fund     ---------------------->               |                 | (3)
B.               <----------------------BC/C+CD/C htlc |                 | (4)
C. CD/C htcl     ---------------------------------------->               | (0)
D.               <---------------------------------------- CD/C htlc D/2 | (1)
E. BC/C htlc     ---->               |                 |                 | sign receiver contract
F.               <---- BC/C htlc B/2 |                 |                 | receiver contract sig
G.BC/C+CD/C B+D/2----------------------->              |                 | (7)
H.   ************** BROADCAST AND MINE CHARLIE FUNDING TX ************** |
I.   C fund      ---------------------------------------->               | (3)
J.               <----------------------------------------CD/D+DA/D htlc | (4)
K. CD/D htlc     ---------------------->               |                 | (E)
L.               <---------------------- CD/D htlc C/2 |                 | (F)
M.CD/D+DA/D C+D/2---------------------------------------->               | (7)
N.   ************** BROADCAST AND MINE DENNIS FUNDING TX *************** |
O. DA/A htlc     ---------------------------------------->               | (E)
P.               <---------------------------------------- DA/A htlc D/2 | (F)
Q. hash preimage ---->               |                 |                 | hash preimage
R.               <---- privB(B+C)    |                 |                 | privkey handover
S.    privA(A+B) ---->               |                 |                 | (R)
T. hash preimage ---------------------->               |                 | (Q)
U.               <---------------------- privC(C+D)    |                 | (R)
V.    privB(B+C) ---------------------->               |                 | (R)
W. hash preimage ---------------------------------------->               | (Q)
X                <---------------------------------------- privD(D+A)    | (R)
Y.    privC(C+D) ---------------------------------------->               | (R)
```

## Code Structure

In the codebase and protocol documentation the words "Sender" and "Receiver" are used. These refer
to either side of a coinswap hop. The entity which created a transaction paying into a coinswap
address is called the sender, because they sent the coins into the coinswap address. The other
entity is called the receiver, because they will receive the coins after the coinswap is complete.

Protocol messages are defined in two enums in the `src/messages.rs`. The individual message names
use `Send` and `Recv` in them to identify their context as per the above definition.

```rust
pub enum MakerToTakerMessage {
    /// Protocol Handshake.
    MakerHello(MakerHello),
    /// Send the Maker's offer advertisement.
    RespOffer(Offer),
    /// Send Contract Sigs **for** the Sender side of the hop. The Maker sending this message is the Receiver of the hop.
    RespContractSigsForSender(ContractSigsForSender),
    /// Request Contract Sigs, **as** both the Sending and Receiving side of the hop.
    ReqContractSigsAsRecvrAndSender(ContractSigsAsRecvrAndSender),
    /// Send Contract Sigs **for** the Receiver side of the hop. The Maker sending this message is the Sender of the hop.
    RespContractSigsForRecvr(ContractSigsForRecvr),
    /// Send the multisig private keys of the swap, declaring completion of the contract.
    RespPrivKeyHandover(PrivKeyHandover),
}
```
```rust
pub enum TakerToMakerMessage {
    /// Protocol Handshake.
    TakerHello(TakerHello),
    /// Request the Maker's Offer advertisement.
    ReqGiveOffer(GiveOffer),
    /// Request Contract Sigs **for** the Sender side of the hop. The Maker receiving this message is the Receiver of the hop.
    ReqContractSigsForSender(ReqContractSigsForSender),
    /// Respond with the [ProofOfFunding] message. This is sent when the funding transaction gets confirmed.
    RespProofOfFunding(ProofOfFunding),
    /// Request Contract Sigs **for** the Receiver and Sender side of the Hop.
    RespContractSigsForRecvrAndSender(ContractSigsForRecvrAndSender),
    /// Request Contract Sigs **for** the Receiver side of the hop. The Maker receiving this message is the Sender of the hop.
    ReqContractSigsForRecvr(ReqContractSigsForRecvr),
    /// Respond with the hash preimage. This settles the HTLC contract. The Receiver side will use this preimage unlock the HTLC.
    RespHashPreimage(HashPreimage),
    /// Respond by handing over the Private Keys of coinswap multisig. This denotes the completion of the whole swap.
    RespPrivKeyHandover(PrivKeyHandover),
}
```

A step-by-step communication sequence with the above messages is provided in `src/messages.rs` [docs](https://github.com/utxo-teleport/teleport-transactions/blob/30be708642cfdaa206d52e147ecb580af7db0bda/src/messages.rs#L20-L59).

The `Taker` carries out all the heavy lifting of the protocol. `Maker`s work like simple state-machine responding to `TakerToMakerMessage`s.

`src/taker.rs` : describes the Taker protocol, which is the workflow defined in the [section above](#protocol-between-takers-and-makers). This is the core of the protocol implementation and the most security-critical section of the library.

`src/maker.rs` : describes the Maker state-machine. This is a simple server responding to various `TakerToMakerMessage`s depending on a `ConnectionState`. Each `ConnectionState` will have specific messages as "allowed". The Maker will terminate the protocol if a received message doesn't match the allowed messages of a specific state.

## Further reading

* [Waxwing's blog post from 2017 about CoinSwap](https://web.archive.org/web/20200524041008/https://joinmarket.me/blog/blog/coinswaps/)

* [gmaxwell's original coinswap writeup from 2013](https://bitcointalk.org/index.php?topic=321228.0). It explains how CoinSwap actually works. If you already understand how Lightning payment channels work then CoinSwap is similar.

* [Design for improving JoinMarket's resistance to sybil attacks using fidelity bonds](https://gist.github.com/chris-belcher/18ea0e6acdb885a2bfbdee43dcd6b5af/). Document explaining the concept of fidelity bonds and how they provide resistance against sybil attacks.



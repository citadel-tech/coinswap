# Example Coinswap with `teleport` cli app

<!-- TOC -->
- [How to create a CoinSwap on `regtest` with yourself](#how-to-create-a-coinswap-on-regtest-with-yourself)
- [How to create a CoinSwap on networks other than `regtest`](#how-to-create-a-coinswap-on-networks-other-than-regtest)
- [How to recover from a failed coinswap](#how-to-recover-from-a-failed-coinswap)
<!-- /TOC -->

## How to create a CoinSwap on `regtest` with yourself

* Start up Bitcoin Core in `regtest` mode. Make sure the RPC server is enabled with `server=1` and that rpc username and password are set with `rpcuser=yourrpcusername` and `rpcpassword=yourrpcpassword` in the configuration file.

* Create three teleport wallets by running `cargo run -- --wallet-file-name=<wallet-name> generate-wallet` thrice. Instead of `<wallet-name>`, use something like `maker1.teleport`, `maker2.teleport` and `taker.teleport`.

* Use `cargo run -- --wallet-file-name=maker1.teleport get-receive-invoice` to obtain 3 addresses of the `maker1` wallet, and send `regtest` bitcoin to each of them (amount 5000000 satoshi or 0.05 BTC in this example). Also do this for the `maker2.teleport` and `taker.teleport` wallets. Get the transactions confirmed.

* Check the wallet balances with `cargo run -- --wallet-file-name=maker1.teleport wallet-balance`. Example:

```console
$ cargo run -- --wallet-file-name=maker1.teleport wallet-balance
coin             address                    type   conf    value
8f6ee5..74e813:0 bcrt1q0vn5....nrjdqljtaq   seed   1       0.05000000 BTC
d548a8..cadd5e:0 bcrt1qaylc....vnw4ay98jq   seed   1       0.05000000 BTC
604ca6..4ab5f0:1 bcrt1qt3jy....df6pmewmzs   seed   1       0.05000000 BTC
coin count = 3
total balance = 0.15000000 BTC
```

```console
$ cargo run -- --wallet-file-name=maker2.teleport wallet-balance
coin             address                    type   conf    value
d33f06..30dd07:0 bcrt1qh6kq....e0tlfrzgxa   seed   1       0.05000000 BTC
8aaa89..ef5613:0 bcrt1q9vyj....plh8x37n7g   seed   1       0.05000000 BTC
383ffe..127065:1 bcrt1qlwzv....pdqtrg0xuu   seed   1       0.05000000 BTC
coin count = 3
total balance = 0.15000000 BTC
```

```console
$ cargo run -- --wallet-file-name=taker.teleport wallet-balance
coin             address                    type   conf    value
5f4331..d53f14:0 bcrt1qmflt....q2ucgf2teu   seed   1       0.05000000 BTC
6252ee..d827b0:0 bcrt1qu9mk....pwpedjyl9u   seed   1       0.05000000 BTC
ac88da..e3ead6:0 bcrt1q3xdx....e7gxtcgrfg   seed   1       0.05000000 BTC
coin count = 3
total balance = 0.15000000 BTC
```

* On another terminal, run a watchtower with `cargo run -- run-watchtower`. You should see the message `Starting teleport watchtower`. In the teleport project, contracts are enforced with one or more watchtowers which are required for the coinswap protocol to be secured against the maker's coins being stolen.

* On one terminal, run a maker server with `cargo run -- --wallet-file-name=maker1.teleport run-yield-generator 6102`. You should see the message `Listening on port 6102`.

* On another terminal, run another maker server with `cargo run -- --wallet-file-name=maker2.teleport run-yield-generator 16102`. You should see the message `Listening on port 16102`.

* On another terminal start a coinswap with `cargo run -- --wallet-file-name=taker.teleport do-coinswap 500000`. When you see the terminal messages `waiting for funding transaction to confirm` and `waiting for maker's funding transaction to confirm` then tell `regtest` to generate another block (or just wait if you're using testnet).

* Once you see the message `successfully completed coinswap` on all terminals then check the wallet balance again to see the result of the coinswap. Example:

```console
$ cargo run -- --wallet-file-name=maker1.teleport wallet-balance
coin             address                    type   conf    value
9bfeec..0cc468:0 bcrt1qx49k....9cqqrp3kt0 swapcoin 2       0.00134344 BTC
973ab4..48f5b7:1 bcrt1qdu4j....ru3qmw4gcf swapcoin 2       0.00224568 BTC
2edf14..74c3b9:0 bcrt1qfw6z....msrsdx9sl0 swapcoin 2       0.00131088 BTC
bd6321..217707:0 bcrt1q35g8....rt6al6kz7s   seed   1       0.04758551 BTC
c6564e..40fb64:0 bcrt1qrnzc....czs840p4np   seed   1       0.04947775 BTC
08e857..c8c67b:0 bcrt1qdxdg....k7882f0ya2   seed   1       0.04808502 BTC
coin count = 6
total balance = 0.15004828 BTC
```

```console
$ cargo run -- --wallet-file-name=maker2.teleport wallet-balance
coin             address                    type   conf    value
9d8895..e32645:1 bcrt1qm73u....3h6swyege3 swapcoin 3       0.00046942 BTC
7cab11..07ff62:1 bcrt1quumg....gtjs29jt8t swapcoin 3       0.00009015 BTC
289a13..ab4672:0 bcrt1qsavn....t5dsac43tl swapcoin 3       0.00444043 BTC
9bfeec..0cc468:1 bcrt1q24f8....443ts4rzz0   seed   2       0.04863932 BTC
973ab4..48f5b7:0 bcrt1q5klz....jhhtlyjpkg   seed   2       0.04773708 BTC
2edf14..74c3b9:1 bcrt1qh2aw....7xx8wft658   seed   2       0.04867188 BTC
coin count = 6
total balance = 0.15004828 BTC
```

```console
$ cargo run -- --wallet-file-name=taker.teleport wallet-balance
coin             address                    type   conf    value
9d8895..e32645:0 bcrt1qevgn....6nhl2yswa7   seed   3       0.04951334 BTC
7cab11..07ff62:0 bcrt1qxs5f....0j8khru45s   seed   3       0.04989261 BTC
289a13..ab4672:1 bcrt1qkwka....g9ts2ch392   seed   3       0.04554233 BTC
bd6321..217707:1 bcrt1qat5h....vytquawwke swapcoin 1       0.00239725 BTC
c6564e..40fb64:1 bcrt1qshwp....3x8qjtwdf6 swapcoin 1       0.00050501 BTC
08e857..c8c67b:1 bcrt1q37lf....5tvqndktw6 swapcoin 1       0.00189774 BTC
coin count = 6
total balance = 0.14974828 BTC
```

## How to create a CoinSwap on networks other than `regtest`

* This is done in pretty much the same way as on the `regtest` network. On public networks you don't always have to coinswap with yourself by creating and funding multiple wallets, instead you could coinswap with other users out there.

* Teleport detects which network it's on by asking the Bitcoin node it's connected to via json-rpc. So to switch between networks like `regtest`, signet, testnet or mainnet (for the brave), make sure the RPC host and port are correct in `src/lib.rs`.

* You will need Tor running on the same machine, then open the file `src/directory_servers.rs` and make sure the const `TOR_ADDR` has the correct Tor port.

* To see all the advertised offers out there, use the `download-offers` subroutine like `cargo run -- download-offers`:

```console
$ cargo run -- download-offers
n   maker address                                                          max size     min size     abs fee      amt rel fee  time rel fee minlocktime
0   5wlgs4tmkc7vmzsqetpjyuz2qbhzydq6d7dotuvbven2cuqjbd2e2oyd.onion:6102    348541       10000        1000         10000000     100000       48
1   eitmocpmxolciziezpp6vzvhufg6djlq2y4oxpm436w5kpzx4tvfgead.onion:16102   314180       10000        1000         10000000     100000       48
```

* To run a yield generator (maker) on any network apart from `regtest`, you will need to create a tor hidden service for your maker. Search the web for "setup tor hidden service", a good article is [this one](https://www.linuxjournal.com/content/tor-hidden-services). When you have your hidden service hostname, copy it into the field near the top of the file `src/maker_protocol.rs`. Run with `cargo run -- --wallet-file-name=maker.teleport run-yield-generator` (note that you can omit the port number, the default port is 6102, specifying a different port number is only really needed for `regtest` where multiple makers are running on the same machine).

* After a successful coinswap created with `do-coinswap`, the coins will still be in the wallet. You can send them out somewhere else using the command `direct-send` and providing the coin(s). For example `cargo run -- --wallet-file-name=taker.teleport direct-send max <destination-address> 9bfeec..0cc468:0`. Coins in the wallet can be found by running `wallet-balance` as above.

## How to recover from a failed coinswap

* CoinSwaps can sometimes fail. Nobody will lose their funds, but they can have their time wasted and have spent miner fees without achieving any privacy gain (or even making their privacy worse, at least until scriptless script contracts are implemented). Everybody is incentivized so that this doesn't happen, and takers are coded to be very persistent in re-establishing a connection with makers before giving up, but sometimes failures will still happen.

* The major way that CoinSwaps can fail is if a taker locks up funds in a 2-of-2 multisig with a maker, but then that maker becomes non-responsive and so the CoinSwap doesn't complete. The taker is left with their money in a multisig and has to use their pre-signed contract transaction to get their money back after a timeout. This section explains how to do that.

* Failed or incomplete coinswaps will show up in wallet display in another section: `cargo run -- --wallet-file-name=taker.teleport wallet-balance`. Example:

```console
$ cargo run -- --wallet-file-name=taker.teleport wallet-balance
= spendable wallet balance =
coin             address                    type   conf    value
9cd867..f80d57:1 bcrt1qgscq....xkxg68mq02   seed   212     0.11103591 BTC
13a0f4..947ab8:1 bcrt1qwfyl....wf0eyf5kuf   seed   212     0.07666832 BTC
901514..10713b:0 bcrt1qghs3....qsg8al2ch4   seed   95      0.04371040 BTC
2fe664..db1a59:0 bcrt1ql83h....hht5vc97dl   seed   94      0.50990000 BTC
coin count = 4
total balance = 0.74131463 BTC
= incomplete coinswaps =
coin             type     preimage locktime/blocks conf    value
10149d..0d0314:1 timelock unknown         9        24      0.00029472 BTC
b36e34..51fa3b:0 timelock unknown         9        24      0.00905248 BTC
2b2e2d..c6db9e:1 timelock unknown         9        24      0.00065280 BTC
outgoing balance = 0.01000000 BTC
hashvalue = a4c2fe816bf18afb8b1861138e57a51bd70e29d4
```

* In this example there is an incomplete coinswap involving three funding transactions, we must take the hashvalue `a4c2fe816bf18afb8b1861138e57a51bd70e29d4` and pass it to the main subroutine: `cargo run -- --wallet-file-name=taker.teleport recover-from-incomplete-coinswap a4c2fe816bf18afb8b1861138e57a51bd70e29d4`.

* Displaying the wallet balance again (`cargo run -- --wallet-file-name=taker.teleport wallet-balance`) after the transactions are broadcast will show the coins in the timelocked contracts section. Example:

```console
$ cargo run -- --wallet-file-name=taker.teleport wallet-balance
= spendable wallet balance =
coin             address                    type   conf    value
9cd867..f80d57:1 bcrt1qgscq....xkxg68mq02   seed   212     0.11103591 BTC
13a0f4..947ab8:1 bcrt1qwfyl....wf0eyf5kuf   seed   212     0.07666832 BTC
901514..10713b:0 bcrt1qghs3....qsg8al2ch4   seed   95      0.04371040 BTC
2fe664..db1a59:0 bcrt1ql83h....hht5vc97dl   seed   94      0.50990000 BTC
coin count = 4
total balance = 0.74131463 BTC
= live timelocked contracts =
coin             hashvalue  timelock conf    locked?  value
452a99..95f364:0 a4c2fe81.. 9        0       locked   0.00904248 BTC
dcfd27..56108a:0 a4c2fe81.. 9        0       locked   0.00064280 BTC
6a8328..f2f5ae:0 a4c2fe81.. 9        0       locked   0.00028472 BTC
```

* Right now these coins are protected by timelocked contracts which are not yet spendable, but after a number of blocks they will be added to the spendable wallet balance, where they can be spent either in a coinswap or with `direct-send`.
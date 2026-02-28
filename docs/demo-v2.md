# Coinswap V2: Live Demo Setup Instructions

Instructions for the full Coinswap setup to be used for live demo.

## Setup The Backend

### Backend Stack

The Coinswap backend stack consists of the following components:
 - bitcoind: Used for wallet syncing and market discovery.
 - tor: Used for all network communications.
 - makerd/maker-cli: The maker server to earn fees from swaps.

### Get The Core Lib

```shell
git clone https://github.com/citadel-tech/coinswap.git
```

### Docker

The backend stack is compiled into an easily usable Docker image.
The containers can be spawned with either a configurable shell script:
```shell
./docker-setup configure
./docker-setup start
```
Or
by using the prebuilt [docker-compose.yml](./../docker-compose.yml) file:
```shell
docker compose up
```

Both will create a backend server stack with Tor and Maker server connected to each other.

For Bitcoind we will use Mutinynet's Signet. For the purpose of the demo, a pre-synced Signet node is 
available for use with RPC and ZMQ access open to the public. The docker compose will automatically connect the maker
server to the pre-synced Mutinynet node.

### Funding The Maker
Once the docker container is up, check out the makerd logs with the following command:
```bash
./docker-setup logs makerd
```
The log will ask for funds to be sent to a specific address. The log will look something like below:
```shell
coinswap-makerd  | 2025-12-01T14:30:26.655863622+00:00 INFO coinswap::wallet::api - Saving wallet to disk: 0 incoming_v2, 0 outgoing_v2 swapcoins
coinswap-makerd  | 2025-12-01T14:30:26.656215409+00:00 INFO coinswap::wallet::rpc - Completed wallet sync and save for "maker-wallet-1764598870"
coinswap-makerd  | 2025-12-01T14:30:27.171543413+00:00 ERROR coinswap::wallet::api - No spendable UTXOs available
coinswap-makerd  | 2025-12-01T14:30:27.172360695+00:00 WARN coinswap::maker::server2 - [6012] Insufficient funds to create fidelity bond.
coinswap-makerd  | 2025-12-01T14:30:27.213539195+00:00 INFO coinswap::watch_tower::rest_backend - Discovery progress: 684/817 blocks scanned (83.7%)
coinswap-makerd  | 2025-12-01T14:30:27.617607128+00:00 INFO coinswap::wallet::api - Saving wallet to disk: 0 incoming_v2, 0 outgoing_v2 swapcoins
coinswap-makerd  | 2025-12-01T14:30:27.617987039+00:00 INFO coinswap::maker::server2 - [6012] Send at least 0.00050324 BTC to tb1qty2ypwf9mllzvsjvwuag3zx3syss2sra82zz9w | If you send extra, that will be added to your wallet balance
```

Use the [Mutinynet Faucet](https://faucet.mutinynet.com/) to send at least 100K sats to the address mentioned in your log.

Once the funds are confirmed, the maker will automatically set up its fidelity bond. Once everything is set up, the maker log will look like below:
```shell
coinswap-makerd  | 2025-12-01T14:35:23.932951431+00:00 INFO coinswap::maker::server2 - [6012] Taproot maker initialized - Address: 3l7nmodhgplgejj3vb42fc5vlfi2zf6hxgbdeepombdlw7ivt5okjrid.onion:6012
coinswap-makerd  | 2025-12-01T14:35:23.937119351+00:00 INFO coinswap::wallet::api - Searching for unfinished swapcoins: 0 incoming, 0 outgoing in store
coinswap-makerd  | 2025-12-01T14:35:23.938856172+00:00 INFO coinswap::maker::server2 - [6012] Taproot maker setup completed
coinswap-makerd  | 2025-12-01T14:35:23.939783495+00:00 INFO coinswap::maker::server2 - [6012] Taproot maker server listening on port 6012
coinswap-makerd  | 2025-12-01T14:35:23.940607635+00:00 INFO coinswap::wallet::api - Searching for unfinished swapcoins: 0 incoming, 0 outgoing in store
coinswap-makerd  | 2025-12-01T14:35:23.941120981+00:00 INFO coinswap::maker::server2 - [6012] Taproot swap liquidity: 49540 sats, 0 ongoing swaps
coinswap-makerd  | 2025-12-01T14:35:23.941453528+00:00 INFO coinswap::maker::rpc::server - [6012] RPC socket binding successful at 127.0.0.1:6013
coinswap-makerd  | 2025-12-01T14:40:23.957061609+00:00 INFO coinswap::wallet::api - Searching for unfinished swapcoins: 0 incoming, 0 outgoing in store
coinswap-makerd  | 2025-12-01T14:40:23.957581168+00:00 INFO coinswap::maker::server2 - [6012] Taproot swap liquidity: 49540 sats, 0 ongoing swaps
```

## Setup The Frontend

Compiling the frontend requires npm and node at the latest version. Use the following commands to install node using nvm, if you don't have it already.

### Get nvm Latest
```shell
curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/master/install.sh | bash
```
### Install npm and node latest
```shell
nvm install node
```
### Check Installation
```shell
node -v
npm -v
```

### Build the Taker App
```shell
git clone https://github.com/citadel-tech/taker-app.git
cd taker-app
npm install
npm run dev
```
## Connecting The Taker App

The Taker app, once started, will try to connect to a bitcoind and Tor network. You can put any value there as per your local node and Tor configuration.

For the purpose of the demo, we will use the following values:

### bitcoind:
 - RPC Host: `172.81.178.3`
 - RPC Port: `48332`
 - RPC User: `user`
 - RPC Password: `password`
 - ZMQ (for both block and tx): `tcp://172.81.178.3:58332`

After inputting the values, click on `Test Connection`. It should return "connection successful".

### Tor:
 - Socks Port: `9050`
 - Control Port: `9051`
 - Tor Auth Password: `coinswap` 

### Create A New Wallet
After setting all the configurations, click on "Create New Wallet". This will start a fresh new Coinswap wallet, make all the connections, and start the UI dashboard.

The rest of the demo will follow the app layout and perform a coinswap across multiple makers.



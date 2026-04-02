# Coinswap Setup

## Maker Dashboard

The Maker Dashboard is a web-based management dashboard to control and manage multiple makers. It is a binary that can be run in any full node environment.

For the purpose of this demo we will use a custom signet, with infinite money and a 30-second block interval. This is compatible with regular bitcoind, but will be our own private blockchain.

Find it here:
- Faucet: http://170.75.166.88:3000/
- Block Explorer: http://170.75.166.88:8080/

### Build the Docker Compose
The docker compose spins up a `bitcoind`, `Tor` and `Maker Dashboard` backend server with all the correct config. It's possible to set it up by hand, but docker is faster.

In a fresh terminal, do:
```shell
git clone git@github.com:citadel-tech/maker-dasboard.git
cd maker-dashboard/docker
docker compose up --build -d
```
Once the docker build completes, connect to `http://127.0.0.1:3000` from a browser to start managing.

### Start and Fund a Maker
Use the usual defaults for bitcoin configurations.

For tor configurations use the following:
- Tor Control Port: `19051`
- Tor Socks Port: `19050`
- Tor Password: `coinswap`

To start a maker:
 - Give basic configuration or use defaults.
 - The fidelity funding address will be displayed and a fund request will be shown.
 - Fund the fidelity address with the requested minimum amount. Makers will not fully start without an established fidelity bond.
 - Wait for fidelity confirmation. Once ready, check out all other maker management options.

### (Optional) Start and Fund Another Maker
It's recommended to have multiple makers running to increase your swap profitability. Just create another maker in the same way.

## Taker Client App

The Taker app is a desktop client app to perform Coinswaps.

### Build or Install

Build the taker desktop app locally.

In a fresh terminal, do:

```bash
# Clone the repository
git clone https://github.com/citadel-tech/taker-app.git
cd taker-app

# Install dependencies and setup native modules
# Note: First-time setup compiles Rust code and may take 2-3 minutes
npm install

# Start development mode
npm run dev
```

Or [download pre-compiled binaries](https://github.com/citadel-tech/taker-app/releases/tag/v0.2.1) for your OS.

After starting the app, use the usual default values for Bitcoin configurations.

Use the following config values to connect the taker-app to the same docker backend:
- Tor Control Port: `19051`
- Tor Socks Port: `19050`
- Tor Password: `coinswap`

If everything goes well, the wallet page will load and all wallet operations will be visible.

### Basic Operations
 - See the marketplace for all available makers. Refresh if you don't see anything.
 - Get a new address.
 - Get some funds from the faucet.
 - Distribute the UTXOs into chunks (optional).

# Perform Swaps
- Select makers from the market.
- Provide swap parameters.
- Start a swap.
- Wait for completion.
- Check the post-swap reports.

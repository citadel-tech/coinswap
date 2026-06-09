<div align="center">

# <img src="https://citadelfoss.xyz/coinswap-bw.svg" alt="Coinswap" height="36" style="vertical-align: middle;"/> Coinswap Live Demo

### Prerequisites & Setup Guide

Get your system ready to participate in the **Coinswap Live Demo** — run a maker, fire up the taker, and perform a real trustless swap on a custom signet.

</div>

---

## 📦 What You'll Be Running

| App | Role | Repository |
| --- | --- | --- |
| 🗄️ **Maker Dashboard** | GUI to run and monitor your maker server | [`citadel-tech/maker-dashboard`](https://github.com/citadel-tech/maker-dashboard) |
| 📱 **Taker App** | GUI client to fund a wallet and perform swaps | [`citadel-tech/taker-app`](https://github.com/citadel-tech/taker-app) |

> 📥 **Recommended — download a pre-compiled binary.** No toolchain required; just grab the binary for your OS from **<https://citadelfoss.xyz/downloads>** and run it.
>
> 🛠️ Prefer to **build from source**? That option is available for each app below (needs the Rust, node, and some more pre-requisites).

---

## ⚙️ System Prerequisites

### 1. Bitcoin Core — *required for everyone*

Download the latest Bitcoin Core from <https://bitcoin.org/en/download>.

Start `bitcoind` with the following `bitcoin.conf`:

```ini
signet=1
[signet]
signetchallenge=0014a3ec9c731da66d9725d54947aede5c830623f33d
addnode=170.75.166.88:38333
dnsseed=0
signetblocktime=30

# RPC configuration for Coinswap operations
server=1
rpcuser=user
rpcpassword=password
rpcport=38332
rpcbind=127.0.0.1
rpcallowip=127.0.0.1

# Enable Bitcoin Core REST interface (used by the watchtower for fast, read-only access)
rest=1

# ZMQ configuration for real-time transaction and block notifications (needed by the watchers)
zmqpubrawblock=tcp://127.0.0.1:28332
zmqpubrawtx=tcp://127.0.0.1:28332

# Required indexes for faster wallet sync
txindex=1
blockfilterindex=1
```

Start `bitcoind` in a dedicated terminal and let it sync.

> 🚰 Use the custom signet **[Faucet](https://faucet.citadelfoss.xyz/)** to grab coins and the **[Block Explorer](https://mempool.citadelfoss.xyz/)** to trace transactions.

> 🧅 **Tor** is bundled into both apps (built via `libtor`) and started automatically — no separate Tor install is needed for the demo.

---

### 2. Build-from-Source Prerequisites — *only if you self-compile*

> ⏭️ **Skip this entire subsection if you're using the pre-compiled binaries.** The tools below are only needed to build the Maker Dashboard or Taker App yourself.

#### Rust & Cargo

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Verify the installation:

```bash
rustc --version
cargo --version
```

#### Node.js & npm

Both apps build their frontend with **Node.js v18 or higher** (which ships with `npm`). Install it from <https://nodejs.org> (or via [`nvm`](https://github.com/nvm-sh/nvm)), then verify:

```bash
node --version   # v18.x or higher
npm --version
```

#### Native build tools for Tor (`libtor`)

Both apps compile and bundle Tor from source via `libtor`, which needs a C toolchain plus autotools, `pkg-config`, and OpenSSL. Install the packages for your platform:

- **Linux (Debian / Ubuntu):**

  ```bash
  sudo apt install build-essential automake autoconf libtool pkg-config libssl-dev
  ```

- **macOS** (via [Homebrew](https://brew.sh)):

  ```sh
  brew install automake autoconf libtool pkg-config openssl
  ```

---

## 🗄️ Maker Dashboard

A GUI for running and monitoring your maker server.

### 📥 Download Pre-compiled Binary (Recommended)

1. Visit **<https://citadelfoss.xyz/downloads>**.
2. Download the **Maker Dashboard** binary for your operating system.
3. Run the binary. Open your browser and navigate to <http://localhost:3000> to access the dashboard.

### 🛠️ Build from Source

> Requires the [build-from-source prerequisites](#2-build-from-source-prerequisites--only-if-you-self-compile) above (Rust, Node.js v18+, and the `libtor` native build tools).

```bash
git clone https://github.com/citadel-tech/maker-dashboard.git
cd maker-dashboard
make build
```

`make build` compiles the Rust backend (`cargo build --release`) and the frontend (`cd frontend && npm install && npm run build`). See the project's [README](https://github.com/citadel-tech/maker-dashboard#readme) for advanced/per-component steps.

---

## 📱 Taker App

A GUI swap client to fund a wallet and perform Coinswaps.

### 📥 Download Pre-compiled Binary (Recommended)

1. Visit **<https://citadelfoss.xyz/downloads>**.
2. Download the **Taker App** binary for your operating system.
3. Run the binary and follow the on-screen onboarding steps.

### 🛠️ Build from Source

> Requires the [build-from-source prerequisites](#2-build-from-source-prerequisites--only-if-you-self-compile) above (Rust, Node.js v18+, and the `libtor` native build tools).

```bash
git clone https://github.com/citadel-tech/taker-app.git
cd taker-app
npm install      # the `prepare` step clones coinswap-ffi and builds the native modules (~2-3 min)
npm run dist     # production build
```

For a live development build, use `npm run dev` instead. See the project's [README](https://github.com/citadel-tech/taker-app#readme) for details.

---

## 🚀 Perform a Swap

1. Launch the **Taker App** and complete onboarding.
2. Fund your wallet using the signet **[Faucet](http://170.75.166.88:3000/)**.
3. Wait for the funding transaction to confirm (track it on the **[Block Explorer](http://170.75.166.88:8080/)**).
4. Pick an available maker from the marketplace and start your swap. 🎉

> 🆘 Stuck during the demo? Flag it to the organizers — and use the block explorer to verify your transactions at any step.

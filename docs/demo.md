<div align="center">

# Coinswap Live Demo

### Prerequisites & Setup Guide

Get your system ready to participate in the **Coinswap Live Demo** — run a maker, fire up the taker, and perform a real trustless swap on a custom signet.

</div>

---

## 📦 What You'll Be Running

| App | Role | Repository |
| --- | --- | --- |
| 🗄️ **Maker Dashboard** | GUI to run and monitor your maker server | [`citadel-tech/maker-dashboard`](https://github.com/citadel-tech/maker-dashboard) |
| 📱 **Taker App** | GUI client to fund a wallet and perform swaps | [`citadel-tech/taker-app`](https://github.com/citadel-tech/taker-app) |

> 🐳 **Maker Dashboard — run with Docker.** No toolchain required; just install Docker and pull the image (see below).
>
> 📥 **Taker App — download a pre-compiled binary.** Grab the binary for your OS from the **[latest release](https://github.com/stark-3k/taker-app/releases/tag/v0.2.2-test1)** and run it.
>
> 🛠️ Prefer to **build from source**? That option is available for each app below (needs Rust, Node.js, and some more pre-requisites).

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

### 2. Docker — *required for the Maker Dashboard*

The Maker Dashboard runs as a Docker container. Install Docker Engine for your platform by following the official guide at <https://docs.docker.com/engine/install/>, then verify:

```bash
docker --version
```

> 💡 On Linux, you may need to add your user to the `docker` group (or prefix commands with `sudo`) to run Docker without root. See <https://docs.docker.com/engine/install/linux-postinstall/>.

---

### 3. Build-from-Source Prerequisites — *only if you self-compile*

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

Both apps compile and bundle Tor from source via `libtor`, which needs a C toolchain plus autotools, `cmake`, `pkg-config`, and OpenSSL. Install the packages for your platform:

- **Linux (Debian / Ubuntu):**

  ```bash
  sudo apt install -y build-essential cmake pkg-config libssl-dev autoconf automake libtool file
  ```

- **macOS** (via [Homebrew](https://brew.sh)):

  ```sh
  brew install automake autoconf libtool pkg-config openssl cmake
  ```

---

## 🗄️ Maker Dashboard

A GUI for running and monitoring your maker server.

### 🐳 Run with Docker (Recommended)

Make sure [Docker is installed](#2-docker--required-for-the-maker-dashboard), then start the dashboard container:

- **Linux:**

  ```bash
  docker run -d --name maker-dashboard --network host \
    --volume ~/.config/maker-dashboard:/root/.config/maker-dashboard \
    --volume ~/.coinswap:/root/.coinswap \
    --env MAKER_DASHBOARD_HOST=127.0.0.1 \
    coinswap/maker-dashboard:master
  ```

- **macOS:**

  Docker Desktop on macOS doesn't support `--network host`, so publish the port explicitly and bind the dashboard to `0.0.0.0` inside the container:

  ```bash
  docker run -d --name maker-dashboard -p 127.0.0.1:3000:3000 \
    --volume ~/.config/maker-dashboard:/root/.config/maker-dashboard \
    --volume ~/.coinswap:/root/.coinswap \
    --env MAKER_DASHBOARD_HOST=0.0.0.0 \
    coinswap/maker-dashboard:master
  ```

Open your browser and navigate to <http://localhost:3000> to access the dashboard.

> 💡 The `--volume` mounts persist your dashboard config and wallet data on the host across container restarts. View logs with `docker logs -f maker-dashboard`, and stop the container with `docker stop maker-dashboard`.

### 🛠️ Build from Source

> Requires the [build-from-source prerequisites](#3-build-from-source-prerequisites--only-if-you-self-compile) above (Rust, Node.js v18+, and the `libtor` native build tools).

```bash
git clone https://github.com/citadel-tech/maker-dashboard.git
cd maker-dashboard
make build
make run
```

`make build` compiles the Rust backend (`cargo build --release`) and the frontend (`cd frontend && npm install && npm run build`). See the project's [README](https://github.com/citadel-tech/maker-dashboard#readme) for advanced/per-component steps.

---

## 📱 Taker App

A GUI swap client to fund a wallet and perform Coinswaps.

### 📥 Download Pre-compiled Binary (Recommended)

1. Visit the **[latest release](https://github.com/0xEgao/taker-app/actions/runs/27262064424)**.
2. Download the **Taker App** binary for your operating system.
3. Run the binary and follow the on-screen onboarding steps.

### 🛠️ Build from Source

> Requires the [build-from-source prerequisites](#3-build-from-source-prerequisites--only-if-you-self-compile) above (Rust, Node.js v18+, and the `libtor` native build tools).

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

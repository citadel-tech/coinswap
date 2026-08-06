<div align="center">

# Coinswap All System Demo

### Prerequisites & Setup Guide

Get your system ready to participate in the **Coinswap Live Demo** — run a maker, fire up the taker, and perform a real trustless swap on a custom signet.

</div>

---

## 📦 What You'll Be Running

| App | Role | Repository |
| --- | --- | --- |
| 🗄️ **Maker Dashboard** | GUI to run and monitor your maker server | [`citadel-tech/maker-dashboard`](https://github.com/citadel-tech/maker-dashboard) |
| 📱 **Taker App** | GUI client to fund a wallet and perform swaps | [`citadel-tech/taker-app`](https://github.com/citadel-tech/taker-app) |

> 📥 **Download a pre-compiled binary (recommended for both apps).** Grab the binary for your OS from each app's **latest release** and run it — no toolchain required.
>
> 🐳 **Maker Dashboard — Docker fallback.** If the binary doesn't work on your system, run the Maker Dashboard as a Docker container instead (see below).
>
> 🛠️ **Build from source (last resort).** If neither of the above works, build either app yourself (needs Rust, Node.js, and some more pre-requisites).

---

## ⚙️ System Prerequisites

### 1. Bitcoin Core — *required for all systems*

Download the latest Bitcoin Core from <https://bitcoin.org/en/download>.

Start `bitcoind` with the following `bitcoin.conf`:

```ini
signet=1
[signet]
# Custom Signet dedicated for the Coinswap Network. 
# This signet is maintained by Citadel FOSS Developers.
signetchallenge=0014a3ec9c731da66d9725d54947aede5c830623f33d
addnode=170.75.166.88:38333
dnsseed=0

# RPC configuration for Coinswap operations
server=1
rpcuser=user
rpcpassword=password
rpcport=38332
rpcbind=127.0.0.1
rpcallowip=127.0.0.1

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

### 2. Docker — *only for the Maker Dashboard Docker fallback*

> ⏭️ **Skip this if the Maker Dashboard binary works for you.** Docker is only needed to run the Maker Dashboard as a container when the pre-compiled binary doesn't work on your system.

Install Docker Engine for your platform by following the official guide at <https://docs.docker.com/engine/install/>, then verify:

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

### 📥 Download Pre-compiled Binary (Recommended)

1. Visit the **[latest release](https://github.com/citadel-tech/maker-dashboard/releases/latest)**.
2. Download the **Maker Dashboard** binary for your operating system.
3. Run the binary and follow the on-screen steps, then open <http://localhost:3000> in your browser to access the dashboard.

### 🐳 Run with Docker (Fallback)

> Use this if the pre-compiled binary doesn't work on your system.

Make sure [Docker is installed](#2-docker--only-for-the-maker-dashboard-docker-fallback), then start the dashboard container:

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

### 🛠️ Build from Source (Last Resort)

> Use this only if the options above don't work. Requires the [build-from-source prerequisites](#3-build-from-source-prerequisites--only-if-you-self-compile) above (Rust, Node.js v18+, and the `libtor` native build tools).

```bash
git clone https://github.com/citadel-tech/maker-dashboard.git
cd maker-dashboard
make build
make run
```

`make build` compiles the Rust backend (`cargo build --release`) and the frontend (`cd frontend && npm install && npm run build`). Once running, open <http://localhost:3000> in your browser. See the project's [README](https://github.com/citadel-tech/maker-dashboard#readme) for advanced/per-component steps.

---

## 📱 Taker App

A GUI swap client to fund a wallet and perform Coinswaps.

### 📥 Download Pre-compiled Binary (Recommended)

1. Visit the **[latest release](https://github.com/citadel-tech/taker-app/releases/latest)**.
2. Download the **Taker App** binary for your operating system.
3. Run the binary and follow the on-screen onboarding steps.

### 🛠️ Build from Source (Last Resort)

> Use this only if the options above don't work. Requires the [build-from-source prerequisites](#3-build-from-source-prerequisites--only-if-you-self-compile) above (Rust, Node.js v18+, and the `libtor` native build tools).

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
2. Fund your wallet using the signet **[Faucet](https://faucet.citadelfoss.xyz/)**.
3. Wait for the funding transaction to confirm (track it on the **[Block Explorer](https://mempool.citadelfoss.xyz/)**).
4. Pick an available maker from the marketplace and start your swap. 🎉

> 🆘 Something didn't work as expected? Please report an [Issue](https://github.com/citadel-tech/coinswap/issues) or ping the devs in the [community forum](https://matrix.to/#/#ciatdel-foss:matrix.org).

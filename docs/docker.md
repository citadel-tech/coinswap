# Docker Setup & Usage Guide

This guide covers how to build and run Coinswap using Docker.

## Overview

The Docker image provides a complete environment for running Coinswap applications with all dependencies included:

- **Alpine Linux 3.18** base image for minimal size
- **Rust 1.75.0** for building the applications
- **Bitcoin Core 26.0** for blockchain operations
- **Tor** for privacy and networking
- All Coinswap binaries: `makerd`, `maker-cli`, `taker`

## Building the Docker Image

```bash
git clone https://github.com/citadel-tech/coinswap.git
cd coinswap
docker build -t coinswap .
```

The build process uses multi-stage builds and caching for optimal performance:
- Dependencies are cached separately from source code
- Rust toolchain and packages are cached
- Bitcoin Core binary is cached
- Test dependencies are built for integration testing

### Build Targets

- **Default**: Production runtime image with all binaries
- **Test**: Development image with test capabilities

```bash
# Build test image
docker build --target test -t coinswap:test .
```

## Running Applications

### Makerd (Maker Daemon)

Run the maker daemon with persistent data storage:

```bash
docker run -d \
  --name coinswap-makerd \
  -p 6102:6102 \
  -p 6103:6103 \
  -v coinswap-maker-data:/home/coinswap/.coinswap \
  -v coinswap-bitcoin-data:/home/coinswap/.bitcoin \
  coinswap makerd
```

**Port mappings:**
- `6102`: Maker network port for coinswap protocol
- `6103`: Maker RPC port for `maker-cli` commands

### Maker CLI

Control the maker daemon:

```bash
# Check maker status
docker run --rm --network host coinswap maker-cli ping

# Get wallet balance
docker run --rm --network host coinswap maker-cli wallet-balance

# Stop the maker daemon
docker run --rm --network host coinswap maker-cli stop
```

### Taker

Run taker operations:

```bash
# Fetch available offers
docker run --rm \
  -v coinswap-taker-data:/home/coinswap/.coinswap \
  coinswap taker fetch-offers

# Send a coinswap
docker run --rm -it \
  -v coinswap-taker-data:/home/coinswap/.coinswap \
  coinswap taker send-coinswap \
  --amount 100000 \
  --destination bc1qexample...
```

## Running Tests

### Integration Tests

Run the complete test suite using Docker:

```bash
# Using the setup script (recommended)
./docker-setup.sh test

# Or manually
docker build --target test -t coinswap:test .
docker run --rm -it coinswap:test
```

The test environment includes:
- All source code and test files
- Pre-built test dependencies
- Integration test features enabled

### Test Data Persistence

Test data can be persisted using volumes:

```bash
docker run --rm -it \
  -v coinswap-test-data:/home/coinswap/.coinswap \
  coinswap:test
```

## Configuration

### Bitcoin Core Configuration

The image includes Bitcoin Core, but you'll need to configure it for your network. Create a bitcoin.conf file:

```bash
# Create a volume for bitcoin configuration
docker volume create bitcoin-config

# Copy your bitcoin.conf to the volume
docker run --rm -v bitcoin-config:/config -v $(pwd):/source alpine cp /source/bitcoin.conf /config/
```

Then run with the configuration:

```bash
docker run -d \
  --name bitcoind \
  -p 18332:18332 \
  -v bitcoin-config:/home/coinswap/.bitcoin \
  -v coinswap-bitcoin-data:/home/coinswap/.bitcoin/signet \
  coinswap bitcoind -signet
```

### Tor Configuration

Tor is pre-installed in the image. For custom Tor configuration:

```bash
# Create custom torrc
echo "SOCKSPort 9050" > torrc

# Run with custom torrc
docker run -d \
  --name coinswap-tor \
  -v $(pwd)/torrc:/etc/tor/torrc:ro \
  coinswap tor
```

## Docker Compose Setup

For a complete setup with all services, create a `docker-compose.yml`:

```yaml
version: '3.8'

services:
  bitcoind:
    image: coinswap
    command: bitcoind -signet -rpcuser=user -rpcpassword=password -rpcallowip=0.0.0.0/0
    ports:
      - "18332:18332"
    volumes:
      - bitcoin-data:/home/coinswap/.bitcoin
    restart: unless-stopped

  tor:
    image: coinswap
    command: tor
    ports:
      - "9050:9050"
    restart: unless-stopped

  directory:
    image: coinswap
    command: directoryd
    ports:
      - "8080:8080"
    volumes:
      - directory-data:/home/coinswap/.coinswap
    depends_on:
      - bitcoind
      - tor
    restart: unless-stopped

  makerd:
    image: coinswap
    command: makerd
    ports:
      - "6102:6102"
      - "6103:6103"
    volumes:
      - maker-data:/home/coinswap/.coinswap
    depends_on:
      - bitcoind
      - tor
      - directory
    restart: unless-stopped

volumes:
  bitcoin-data:
  directory-data:
  maker-data:
```

Run the complete stack:

```bash
docker-compose up -d
```

## Data Persistence

All application data is stored in Docker volumes:

- `coinswap-maker-data`: Maker configuration and wallet data
- `coinswap-taker-data`: Taker wallet data
- `coinswap-bitcoin-data`: Bitcoin blockchain data
- `coinswap-directory-data`: Directory server data

## Troubleshooting

### Check logs

```bash
# Makerd logs
docker logs coinswap-makerd

# Follow logs in real-time
docker logs -f coinswap-makerd
```

### Interactive debugging

```bash
# Enter container for debugging
docker run --rm -it coinswap sh

# Check binary locations
docker run --rm coinswap ls -la /app/bin/
```

### Network connectivity

```bash
# Test Tor connectivity
docker run --rm coinswap curl -x socks5h://localhost:9050 http://check.torproject.org

# Test Bitcoin Core connection
docker run --rm coinswap bitcoin-cli -signet getblockchaininfo
```

## Security Considerations

- The container runs as a non-root user (`coinswap:1001`)
- Private keys are stored in Docker volumes - ensure proper backup
- For production use, consider using Docker secrets for sensitive configuration
- Network isolation can be achieved using custom Docker networks

## Performance Tips

- Use Docker BuildKit for faster builds: `DOCKER_BUILDKIT=1 docker build`
- Mount cargo cache for development: `-v cargo-cache:/root/.cargo`
- Use multi-stage builds to minimize final image size
- Consider using `docker system prune` to clean up build cache

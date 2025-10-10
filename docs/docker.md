# Docker Setup & Usage Guide

This guide covers how to build and run Coinswap using Docker with a modern multi-service architecture.

## Overview

The Docker setup provides a complete environment for running ```bash
docker run --rm -it 
  -v coinswap-test-data:/home/coinswap/.coinswap 
  coinswap-test
```

## Docker Compose Setup

The setup script automatically generates `docker-compose.generated.yml` based on your configuration. For a complete setup with all services:

```bash
# Start all services (Bitcoin Core, Tor, Tracker, Makerd)
./docker-setup.sh start

# Or use docker-compose directly
docker-compose -f docker-compose.generated.yml up -d

# Check status
./docker-setup.sh status

# View logs
./docker-setup.sh logs makerd
```

### Example Generated Configuration

The setup script creates configuration like this:

```yaml
version: '3.8'

services:
  bitcoind:
    image: bitcoind/bitcoin-core:25.0
    container_name: coinswap-bitcoind
    command: |
      bitcoind
      -regtest=1
      -server=1
      -rpcuser=coinswap
      -rpcpassword=coinswappass
      -rpcallowip=0.0.0.0/0
      -txindex=1
    ports:
      - "18332:18332"
    volumes:
      - bitcoin-data:/home/bitcoin/.bitcoin

  tor:
    image: torproject/tor:latest
    container_name: coinswap-tor
    volumes:
      - ./docker/torrc:/etc/tor/torrc:ro
    ports:
      - "9050:9050"
      - "9051:9051"

  tracker:
    build:
      context: .
      dockerfile: docker/Dockerfile.tracker
    image: coinswap-tracker
    ports:
      - "8080:8080"
    depends_on:
      - bitcoind
      - tor

  makerd:
    build:
      context: .
      dockerfile: docker/Dockerfile.maker
    image: coinswap-maker
    ports:
      - "6102:6102"
      - "6103:6103"
    depends_on:
      - bitcoind
      - tor
      - tracker
```nswap applications with service-specific containers:

- **Service-specific Dockerfiles** for optimal maintainability and efficiency
- **Alpine Linux 3.20** base images for minimal size
- **Rust 1.90.0** for building the applications
- **External images** for Bitcoin Core and Tor
- **Interactive configuration** with automatic service detection
- All Coinswap binaries: `makerd`, `maker-cli`, `taker`, `tracker`

## Architecture

The Docker setup uses multiple specialized containers:

- `docker/Dockerfile.builder` - Shared build stage for all Rust binaries
- `docker/Dockerfile.maker` - Maker daemon service
- `docker/Dockerfile.taker` - Taker client service  
- `docker/Dockerfile.tracker` - Tracker discovery service
- `docker/Dockerfile.test` - Test environment with all binaries
- External images: `bitcoind/bitcoin-core` and `torproject/tor`

## Quick Start

### Using the Setup Script (Recommended)

The `docker-setup.sh` script provides an interactive way to configure and run Coinswap:

```bash
git clone https://github.com/citadel-tech/coinswap.git
cd coinswap

# Interactive configuration
./docker-setup.sh configure

# Build all Docker images
./docker-setup.sh build

# Start the complete stack
./docker-setup.sh start

# Check status
./docker-setup.sh status

# View logs
./docker-setup.sh logs makerd
```

### Configuration Options

The setup script will prompt for:

1. **Bitcoin Core Configuration**:
   - Data directory path
   - Network selection (regtest/signet/testnet/mainnet)
   - Use existing Bitcoin Core instance or spawn new one
   - Custom RPC ports

2. **Tor Configuration**:
   - Detect existing Tor instance
   - Use external Tor or spawn containerized version
   - Custom SOCKS and control ports

3. **Service Ports**:
   - Makerd network port (default: 6102)
   - Makerd RPC port (default: 6103)  
   - Tracker port (default: 8080)

Configuration is saved to `.docker-config` and reused on subsequent runs.

## Building Docker Images

The build process uses a shared builder stage and service-specific images:

```bash
git clone https://github.com/citadel-tech/coinswap.git
cd coinswap

# Build all images using the setup script
./docker-setup.sh build

# Or build manually
docker build -f docker/Dockerfile.builder -t coinswap-builder .
docker build -f docker/Dockerfile.maker --build-arg BUILDER_IMAGE=coinswap-builder -t coinswap-maker .
docker build -f docker/Dockerfile.taker --build-arg BUILDER_IMAGE=coinswap-builder -t coinswap-taker .
docker build -f docker/Dockerfile.tracker --build-arg BUILDER_IMAGE=coinswap-builder -t coinswap-tracker .
```

The build process uses multi-stage builds and caching for optimal performance:
- Shared builder stage compiles all Rust binaries and the tracker
- Dependencies are cached separately from source code
- Service-specific images only include necessary binaries
- Each service gets a minimal Alpine runtime image

### Available Images

- **coinswap-builder**: Shared builder with all binaries
- **coinswap-maker**: Maker daemon service (`makerd`, `maker-cli`)
- **coinswap-taker**: Taker client service (`taker`)
- **coinswap-tracker**: Tracker discovery service (`tracker`)
- **coinswap-test**: Test environment with all binaries

## Running Applications

### Makerd (Maker Daemon)

Run the maker daemon with persistent data storage:

```bash
# Using the setup script (recommended)
./docker-setup.sh start

# Or manually with specific image
docker run -d 
  --name coinswap-makerd 
  -p 6102:6102 
  -p 6103:6103 
  -v coinswap-maker-data:/home/coinswap/.coinswap 
  coinswap-maker
```

**Port mappings:**
- `6102`: Maker network port for coinswap protocol
- `6103`: Maker RPC port for `maker-cli` commands

### Maker CLI

Control the maker daemon:

```bash
# Using the setup script
./docker-setup.sh maker-cli ping
./docker-setup.sh maker-cli wallet-balance
./docker-setup.sh maker-cli stop

# Or manually
docker run --rm --network coinswap-network coinswap-maker maker-cli ping
docker run --rm --network coinswap-network coinswap-maker maker-cli wallet-balance
docker run --rm --network coinswap-network coinswap-maker maker-cli stop
```

### Taker

Run taker operations:

```bash
# Using the setup script
./docker-setup.sh taker fetch-offers
./docker-setup.sh taker --help

# Or manually
docker run --rm 
  -v coinswap-taker-data:/home/coinswap/.coinswap 
  --network coinswap-network 
  coinswap-taker fetch-offers

docker run --rm -it 
  -v coinswap-taker-data:/home/coinswap/.coinswap 
  --network coinswap-network 
  coinswap-taker --help
```

### Tracker

The tracker service runs automatically as part of the stack:

```bash
# Check tracker logs
./docker-setup.sh logs tracker

# Run tracker manually
docker run -d 
  --name coinswap-tracker 
  -p 8080:8080 
  -v coinswap-tracker-data:/home/coinswap/.tracker 
  coinswap-tracker
```

## Running Tests

### Integration Tests

Run the complete test suite using Docker:

```bash
# Using the setup script (recommended)
./docker-setup.sh test

# Or manually
docker build -f docker/Dockerfile.builder -t coinswap-builder .
docker build -f docker/Dockerfile.test --build-arg BUILDER_IMAGE=coinswap-builder -t coinswap-test .
docker run --rm -it coinswap-test
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

## Data Persistence

All application data is stored in Docker volumes:

- `bitcoin-data`: Bitcoin blockchain data
- `tor-data`: Tor configuration and data
- `tracker-data`: Tracker discovery service data
- `maker-data`: Maker configuration and wallet data
- `taker-data`: Taker wallet data (when using manual commands)

## Troubleshooting

### Check logs

```bash
# Using setup script
./docker-setup.sh logs makerd
./docker-setup.sh logs tracker

# Or directly with docker-compose
docker-compose -f docker-compose.generated.yml logs -f makerd
```

### Interactive debugging

```bash
# Enter container for debugging
./docker-setup.sh shell

# Or manually
docker run --rm -it coinswap-maker sh
```

### Network connectivity

```bash
# Test maker connectivity
./docker-setup.sh maker-cli ping

# Test taker functionality
./docker-setup.sh taker fetch-offers
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

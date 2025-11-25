#!/bin/bash

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IMAGE_NAME="coinswap"
CONFIG_FILE="$SCRIPT_DIR/.docker-config"

DEFAULT_BITCOIN_DATADIR="/home/coinswap/.bitcoin"
DEFAULT_BITCOIN_NETWORK="signet"
DEFAULT_BITCOIN_RPC_PORT="38332"
DEFAULT_BITCOIN_ZMQ_PORT="28332"
DEFAULT_MAKERD_PORT="6102"
DEFAULT_MAKERD_RPC_PORT="6103"
DEFAULT_TOR_SOCKS_PORT="9050"
DEFAULT_TOR_CONTROL_PORT="9051"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'
print_info() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

print_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $1"
}

print_warning() {
    echo -e "${YELLOW}[WARNING]${NC} $1"
}

print_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

load_config() {
    if [ -f "$CONFIG_FILE" ]; then
        source "$CONFIG_FILE"
        print_info "Loaded configuration from $CONFIG_FILE"
    fi
}

save_config() {
    cat > "$CONFIG_FILE" << EOF
# Coinswap Docker Configuration
BITCOIN_DATADIR="$BITCOIN_DATADIR"
BITCOIN_NETWORK="$BITCOIN_NETWORK"
BITCOIN_RPC_PORT="$BITCOIN_RPC_PORT"
BITCOIN_ZMQ_PORT="$BITCOIN_ZMQ_PORT"
MAKERD_PORT="$MAKERD_PORT"
MAKERD_RPC_PORT="$MAKERD_RPC_PORT"
TOR_SOCKS_PORT="$TOR_SOCKS_PORT"
TOR_CONTROL_PORT="$TOR_CONTROL_PORT"
USE_EXTERNAL_BITCOIND="$USE_EXTERNAL_BITCOIND"
USE_EXTERNAL_TOR="$USE_EXTERNAL_TOR"
EXTERNAL_BITCOIND_HOST="$EXTERNAL_BITCOIND_HOST"
EXTERNAL_TOR_HOST="$EXTERNAL_TOR_HOST"
EOF
    print_success "Configuration saved to $CONFIG_FILE"
}

check_port() {
    local port=$1
    if nc -z localhost "$port" 2>/dev/null; then
        return 1
    else
        return 0
    fi
}

check_tor_running() {
    if check_port 9050; then
        return 1
    else
        return 0
    fi
}

check_bitcoind_running() {
    local port=${1:-18332}
    if check_port "$port"; then
        return 1
    else
        return 0
    fi
}

configure_setup() {
    echo ""
    print_info "Coinswap Docker Configuration"
    echo "============================================"
    echo ""
    
    print_info "Bitcoin Core Configuration"
    echo "----------------------------------------"
    
    read -p "Bitcoin data directory [${DEFAULT_BITCOIN_DATADIR}]: " input
    BITCOIN_DATADIR="${input:-$DEFAULT_BITCOIN_DATADIR}"
    
    echo ""
    echo "Select Bitcoin network:"
    echo "1) signet (default - test network)"
    echo "2) regtest (for testing)"
    echo "3) testnet (test network)"  
    echo "4) mainnet (CAUTION: real bitcoin)"
    read -p "Network [1]: " network_choice
    
    case "${network_choice:-1}" in
        1) BITCOIN_NETWORK="signet"; BITCOIN_RPC_PORT="38332"; BITCOIN_ZMQ_PORT="28332" ;;
        2) BITCOIN_NETWORK="regtest"; BITCOIN_RPC_PORT="18442"; BITCOIN_ZMQ_PORT="28332" ;;
        3) BITCOIN_NETWORK="testnet"; BITCOIN_RPC_PORT="18332"; BITCOIN_ZMQ_PORT="28332" ;;
        4) BITCOIN_NETWORK="mainnet"; BITCOIN_RPC_PORT="8332"; BITCOIN_ZMQ_PORT="28332" ;;
        *) BITCOIN_NETWORK="regtest"; BITCOIN_RPC_PORT="18442"; BITCOIN_ZMQ_PORT="28332" ;;
    esac
    
    echo ""
    if check_bitcoind_running "$BITCOIN_RPC_PORT"; then
        print_info "Detected Bitcoin Core running on port $BITCOIN_RPC_PORT"
        read -p "Use existing Bitcoin Core instance? [Y/n]: " use_existing
        if [[ "${use_existing:-Y}" =~ ^[Yy]$ ]]; then
            USE_EXTERNAL_BITCOIND="true"
            read -p "Bitcoin RPC host [localhost:$BITCOIN_RPC_PORT]: " btc_host
            EXTERNAL_BITCOIND_HOST="${btc_host:-localhost:$BITCOIN_RPC_PORT}"
        else
            USE_EXTERNAL_BITCOIND="false"
            read -p "Bitcoin RPC port [$BITCOIN_RPC_PORT]: " btc_port
            BITCOIN_RPC_PORT="${btc_port:-$BITCOIN_RPC_PORT}"
            read -p "Bitcoin ZMQ port [$BITCOIN_ZMQ_PORT]: " btc_zmq_port
            BITCOIN_ZMQ_PORT="${btc_zmq_port:-$BITCOIN_ZMQ_PORT}"
        fi
    else
        USE_EXTERNAL_BITCOIND="false"
        read -p "Bitcoin RPC port [$BITCOIN_RPC_PORT]: " btc_port
        BITCOIN_RPC_PORT="${btc_port:-$BITCOIN_RPC_PORT}"
        read -p "Bitcoin ZMQ port [$BITCOIN_ZMQ_PORT]: " btc_zmq_port
        BITCOIN_ZMQ_PORT="${btc_zmq_port:-$BITCOIN_ZMQ_PORT}"
    fi
    
    echo ""
    print_info "Tor Configuration"
    echo "----------------------------------------"
    
    if check_tor_running; then
        print_info "Detected Tor running on port 9050"
        read -p "Use existing Tor instance? [Y/n]: " use_existing_tor
        if [[ "${use_existing_tor:-Y}" =~ ^[Yy]$ ]]; then
            USE_EXTERNAL_TOR="true"
            read -p "Tor SOCKS host [localhost:9050]: " tor_host
            EXTERNAL_TOR_HOST="${tor_host:-localhost:9050}"
        else
            USE_EXTERNAL_TOR="false"
            read -p "Tor SOCKS port [${DEFAULT_TOR_SOCKS_PORT}]: " tor_socks
            TOR_SOCKS_PORT="${tor_socks:-$DEFAULT_TOR_SOCKS_PORT}"
            read -p "Tor control port [${DEFAULT_TOR_CONTROL_PORT}]: " tor_control
            TOR_CONTROL_PORT="${tor_control:-$DEFAULT_TOR_CONTROL_PORT}"
        fi
    else
        USE_EXTERNAL_TOR="false"
        read -p "Tor SOCKS port [${DEFAULT_TOR_SOCKS_PORT}]: " tor_socks
        TOR_SOCKS_PORT="${tor_socks:-$DEFAULT_TOR_SOCKS_PORT}"
        read -p "Tor control port [${DEFAULT_TOR_CONTROL_PORT}]: " tor_control
        TOR_CONTROL_PORT="${tor_control:-$DEFAULT_TOR_CONTROL_PORT}"
    fi
    
    echo ""
    print_info "Coinswap Service Ports"
    echo "----------------------------------------"
    
    read -p "Makerd network port [${DEFAULT_MAKERD_PORT}]: " makerd_port
    MAKERD_PORT="${makerd_port:-$DEFAULT_MAKERD_PORT}"
    
    read -p "Makerd RPC port [${DEFAULT_MAKERD_RPC_PORT}]: " makerd_rpc
    MAKERD_RPC_PORT="${makerd_rpc:-$DEFAULT_MAKERD_RPC_PORT}"
    
    echo ""
    print_info "Configuration Summary"
    echo "----------------------------------------"
    echo "Bitcoin Network: $BITCOIN_NETWORK"
    echo "Bitcoin Data Dir: $BITCOIN_DATADIR"
    echo "Bitcoin RPC Port: $BITCOIN_RPC_PORT"
    echo "Bitcoin ZMQ Port: $BITCOIN_ZMQ_PORT"
    echo "Use External Bitcoin: $USE_EXTERNAL_BITCOIND"
    if [[ "$USE_EXTERNAL_BITCOIND" == "true" ]]; then
        echo "External Bitcoin Host: $EXTERNAL_BITCOIND_HOST"
    fi
    echo "Use External Tor: $USE_EXTERNAL_TOR"
    if [[ "$USE_EXTERNAL_TOR" == "true" ]]; then
        echo "External Tor Host: $EXTERNAL_TOR_HOST"
    else
        echo "Tor SOCKS Port: $TOR_SOCKS_PORT"
        echo "Tor Control Port: $TOR_CONTROL_PORT"
    fi
    echo "Makerd Port: $MAKERD_PORT"
    echo "Makerd RPC Port: $MAKERD_RPC_PORT"
    echo ""
    
    read -p "Save this configuration? [Y/n]: " save_config_prompt
    if [[ "${save_config_prompt:-Y}" =~ ^[Yy]$ ]]; then
        save_config
    fi
}

check_docker() {
    if ! command -v docker &> /dev/null; then
        print_error "Docker is not installed. Please install Docker first."
        exit 1
    fi
    
    if ! docker info &> /dev/null; then
        print_error "Docker is not running. Please start Docker first."
        exit 1
    fi
    
    print_success "Docker is available and running"
}

generate_compose_file() {
    local compose_file="$SCRIPT_DIR/docker-compose.generated.yml"
    
    print_info "Generating docker-compose configuration..."
    
    cat > "$compose_file" << EOF
services:
EOF

    if [[ "$USE_EXTERNAL_BITCOIND" != "true" ]]; then
        cat >> "$compose_file" << EOF
  bitcoind:
    image: bitcoin/bitcoin:master-alpine
    container_name: coinswap-bitcoind
    command: |
      bitcoind
      -${BITCOIN_NETWORK}=1
      -server=1
      -fallbackfee=0.0001
      -rpcuser=user
      -rpcpassword=password
      -rpcallowip=0.0.0.0/0
      -rpcbind=0.0.0.0:$BITCOIN_RPC_PORT
      -zmqpubrawblock=tcp://0.0.0.0:$BITCOIN_ZMQ_PORT
      -zmqpubrawtx=tcp://0.0.0.0:$BITCOIN_ZMQ_PORT
      -txindex=1
      -blockfilterindex=1
      -datadir=/home/bitcoin/.bitcoin
      -signetchallenge=0014c9e9f8875a25c3cc6d99ad3e5fd54254d00fed44
      -addnode=172.81.178.3:38333
    ports:
      - "$BITCOIN_RPC_PORT:$BITCOIN_RPC_PORT"
      - "$BITCOIN_ZMQ_PORT:$BITCOIN_ZMQ_PORT"
    volumes:
      - bitcoin-data:/home/bitcoin/.bitcoin
    restart: unless-stopped
    healthcheck:
      test: ["CMD", "bitcoin-cli", "-rpcuser=user", "-rpcpassword=password", "getblockchaininfo"]
      interval: 30s
      timeout: 10s
      retries: 5

EOF
    fi

    if [[ "$USE_EXTERNAL_TOR" != "true" ]]; then
        cat >> "$compose_file" << EOF
  tor:
    image: torproject/tor:latest
    container_name: coinswap-tor
    volumes:
      - tor-data:/var/lib/tor
      - ./docker/torrc:/etc/tor/torrc:ro
    ports:
      - "$TOR_SOCKS_PORT:9050"
      - "$TOR_CONTROL_PORT:9051"
    restart: unless-stopped

EOF
    fi

    cat >> "$compose_file" << EOF
  makerd:
    image: ${IMAGE_NAME}:latest
    pull_policy: never
    container_name: coinswap-makerd
    command: |
      sh -c "
      sleep 15 &&
      mkdir -p /home/coinswap/.coinswap/maker &&
      cat > /home/coinswap/.coinswap/maker/config.toml << EOM
      network_port = $MAKERD_PORT
      rpc_port = $MAKERD_RPC_PORT
      socks_port = $TOR_SOCKS_PORT
      control_port = $TOR_CONTROL_PORT
      tor_auth_password = \"\"
      min_swap_amount = 10000
      fidelity_amount = 50000
      fidelity_timelock = 13104
      connection_type = \"TOR\"
      base_fee = 100
      amount_relative_fee_ppt = 1000
      EOM
      makerd --rpc bitcoind:$BITCOIN_RPC_PORT --auth coinswap:coinswappass
      "
    ports:
      - "$MAKERD_PORT:$MAKERD_PORT"
      - "$MAKERD_RPC_PORT:$MAKERD_RPC_PORT"
    volumes:
      - maker-data:/home/coinswap/.coinswap
    environment:
      - RUST_LOG=info
EOF

    # Add makerd dependencies  
    local makerd_deps=()
    [[ "$USE_EXTERNAL_BITCOIND" != "true" ]] && makerd_deps+=("bitcoind")
    [[ "$USE_EXTERNAL_TOR" != "true" ]] && makerd_deps+=("tor")
    
    cat >> "$compose_file" << EOF
    depends_on:
EOF
    for dep in "${makerd_deps[@]}"; do
        cat >> "$compose_file" << EOF
      - $dep
EOF
    done
    
    cat >> "$compose_file" << EOF
    restart: unless-stopped

volumes:
EOF

    if [[ "$USE_EXTERNAL_BITCOIND" != "true" ]]; then
        cat >> "$compose_file" << EOF
  bitcoin-data:
    driver: local
EOF
    fi
    
    if [[ "$USE_EXTERNAL_TOR" != "true" ]]; then
        cat >> "$compose_file" << EOF
  tor-data:
    driver: local
EOF
    fi

    cat >> "$compose_file" << EOF
  maker-data:
    driver: local

networks:
  default:
    name: coinswap-network
EOF

    print_success "Generated docker-compose configuration: $compose_file"
}

# Build the Docker image
build_image() {
    print_info "Building Coinswap Docker image..."
    cd "$SCRIPT_DIR"
    
    if docker build -f docker/Dockerfile -t "${IMAGE_NAME}:latest" .; then
        print_success "Coinswap image built successfully"
    else
        print_error "Failed to build Coinswap image"
        exit 1
    fi
}

# Publish the Docker image
publish_image() {
    local version="$1"
    local username="${DOCKERHUB_USERNAME}"
    
    if [ -z "$username" ]; then
        read -p "Enter Docker Hub username: " username
    fi
    
    if [ -z "$username" ]; then
        print_error "Username required to publish"
        exit 1
    fi

    local repo="${username}/${IMAGE_NAME}"
    
    # Ensure image is built
    build_image
    
    print_info "Tagging and pushing image to $repo..."
    
    # Tag and push latest
    docker tag "${IMAGE_NAME}:latest" "${repo}:latest"
    docker push "${repo}:latest"
    
    # Tag and push version if provided
    if [ -n "$version" ]; then
        docker tag "${IMAGE_NAME}:latest" "${repo}:${version}"
        docker push "${repo}:${version}"
        print_success "Published ${repo}:${version}"
    fi
    
    print_success "Published ${repo}:latest"
}

# Start the full stack using docker-compose
start_stack() {
    local use_defaults="false"

    while [[ $# -gt 0 ]]; do
        case $1 in
            -d|--default)
                use_defaults="true"
                shift
                ;;
            *)
                shift
                ;;
        esac
    done

    # Load existing configuration or prompt for new one
    load_config
    
    # If no configuration exists, run configuration setup
    if [[ -z "$BITCOIN_DATADIR" ]]; then
        if [[ "$use_defaults" == "true" ]]; then
            print_info "Using default configuration..."
        else
            configure_setup
        fi
    fi
    
    # Set defaults for any unset variables
    BITCOIN_DATADIR="${BITCOIN_DATADIR:-$DEFAULT_BITCOIN_DATADIR}"
    BITCOIN_NETWORK="${BITCOIN_NETWORK:-$DEFAULT_BITCOIN_NETWORK}"
    BITCOIN_RPC_PORT="${BITCOIN_RPC_PORT:-$DEFAULT_BITCOIN_RPC_PORT}"
    BITCOIN_ZMQ_PORT="${BITCOIN_ZMQ_PORT:-$DEFAULT_BITCOIN_ZMQ_PORT}"
    MAKERD_PORT="${MAKERD_PORT:-$DEFAULT_MAKERD_PORT}"
    MAKERD_RPC_PORT="${MAKERD_RPC_PORT:-$DEFAULT_MAKERD_RPC_PORT}"
    TOR_SOCKS_PORT="${TOR_SOCKS_PORT:-$DEFAULT_TOR_SOCKS_PORT}"
    TOR_CONTROL_PORT="${TOR_CONTROL_PORT:-$DEFAULT_TOR_CONTROL_PORT}"
    USE_EXTERNAL_BITCOIND="${USE_EXTERNAL_BITCOIND:-false}"
    USE_EXTERNAL_TOR="${USE_EXTERNAL_TOR:-false}"
    
    # Generate dynamic docker-compose file
    generate_compose_file
    
    print_info "Starting Coinswap stack with docker-compose..."
    cd "$SCRIPT_DIR"
    
    if docker compose -f docker-compose.generated.yml up -d; then
        print_success "Coinswap stack started successfully"
        print_info "Services running:"
        docker compose -f docker-compose.generated.yml ps
    else
        print_error "Failed to start Coinswap stack"
        exit 1
    fi
}

stop_stack() {
    print_info "Stopping Coinswap stack..."
    cd "$SCRIPT_DIR"
    
    # Use generated compose file if it exists, otherwise fall back to default
    if [ -f "docker-compose.generated.yml" ]; then
        docker compose -f docker-compose.generated.yml down
    else
        docker compose down
    fi
    
    print_success "Coinswap stack stopped"
}

show_logs() {
    cd "$SCRIPT_DIR"
    
    local compose_file="docker-compose.generated.yml"
    if [ ! -f "$compose_file" ]; then
        compose_file="docker-compose.yml"
    fi
    
    if [ -n "$1" ]; then
        docker compose -f "$compose_file" logs -f "$1"
    else
        docker compose -f "$compose_file" logs -f
    fi
}

run_command() {
    local cmd="$1"
    shift
    docker run --rm -it --network coinswap-network "${IMAGE_NAME}:latest" "$cmd" "$@"
}

show_help() {
    echo "Coinswap Docker Setup Script"
    echo ""
    echo "Usage: $0 [COMMAND]"
    echo ""
    echo "Commands:"
    echo "  configure        Configure Coinswap Docker setup"
    echo "  build            Build the Docker image"
    echo "  publish [ver]    Publish image to Docker Hub (optional version tag)"
    echo "  start [options]  Start the full Coinswap stack"
    echo "                   Options:"
    echo "                     -d, --default   Use default configuration without prompting"
    echo "  stop             Stop the Coinswap stack"
    echo "  restart          Restart the Coinswap stack"
    echo "  logs [service]   Show logs (optionally for specific service)"
    echo "  status           Show status of running services"
    echo "  shell            Open shell in a new container"
    echo ""
    echo "Individual application commands:"
    echo "  makerd [args]   Run makerd with arguments"
    echo "  maker-cli [args] Run maker-cli with arguments"
    echo "  taker [args]    Run taker with arguments"
    echo "  bitcoin-cli [args] Run bitcoin-cli with arguments"
    echo ""
    echo "Examples:"
    echo "  $0 build"
    echo "  $0 publish 1.0.0"
    echo "  $0 start"
    echo "  $0 taker --help"
    echo "  $0 maker-cli ping"
    echo "  $0 logs makerd"
}

case "${1:-}" in
    "configure")
        configure_setup
        ;;
    "build")
        check_docker
        build_image
        ;;
    "publish")
        check_docker
        publish_image "$2"
        ;;
    "start")
        check_docker
        start_stack "${@:2}"
        ;;
    "stop")
        stop_stack
        ;;
    "restart")
        stop_stack
        sleep 2
        start_stack
        ;;
    "logs")
        show_logs "$2"
        ;;
    "status")
        cd "$SCRIPT_DIR"
        compose_file="docker-compose.generated.yml"
        if [ ! -f "$compose_file" ]; then
            compose_file="docker-compose.yml"
        fi
        docker compose -f "$compose_file" ps
        ;;
    "shell")
        docker run --rm -it --network coinswap-network "${IMAGE_NAME}:latest" /bin/sh
        ;;
    "makerd"|"maker-cli"|"taker"|"bitcoin-cli")
        run_command "$@"
        ;;
    "help"|"--help"|"-h"|"")
        show_help
        ;;
    *)
        print_error "Unknown command: $1"
        echo ""
        show_help
        exit 1
        ;;
esac

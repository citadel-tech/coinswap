#!/bin/bash

# Coinswap Docker Setup Script
# This script helps set up and run the Coinswap environment using Docker

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IMAGE_NAME="coinswap"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Print colored output
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

# Check if Docker is installed and running
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

# Build the Docker image
build_image() {
    print_info "Building Coinswap Docker image..."
    cd "$SCRIPT_DIR"
    
    if docker build -t "$IMAGE_NAME" .; then
        print_success "Docker image built successfully"
    else
        print_error "Failed to build Docker image"
        exit 1
    fi
}

# Start the full stack using docker-compose
start_stack() {
    print_info "Starting Coinswap stack with docker-compose..."
    cd "$SCRIPT_DIR"
    
    if docker-compose up -d; then
        print_success "Coinswap stack started successfully"
        print_info "Services running:"
        docker-compose ps
    else
        print_error "Failed to start Coinswap stack"
        exit 1
    fi
}

# Stop the stack
stop_stack() {
    print_info "Stopping Coinswap stack..."
    cd "$SCRIPT_DIR"
    docker-compose down
    print_success "Coinswap stack stopped"
}

# Show logs
show_logs() {
    cd "$SCRIPT_DIR"
    if [ -n "$1" ]; then
        docker-compose logs -f "$1"
    else
        docker-compose logs -f
    fi
}

# Run individual commands
run_command() {
    local cmd="$1"
    shift
    docker run --rm -it --network coinswap-network "$IMAGE_NAME" "$cmd" "$@"
}

# Run tests
run_tests() {
    print_info "Running Coinswap tests in Docker..."
    
    # Build the test stage
    print_info "Building test image..."
    if docker build --target test -t "$IMAGE_NAME:test" .; then
        print_success "Test image built successfully"
    else
        print_error "Failed to build test image"
        exit 1
    fi
    
    # Run the tests
    print_info "Executing integration tests..."
    docker run --rm -it \
        --network coinswap-network \
        -v coinswap-test-data:/home/coinswap/.coinswap \
        "$IMAGE_NAME:test"
}

# Show help
show_help() {
    echo "Coinswap Docker Setup Script"
    echo ""
    echo "Usage: $0 [COMMAND]"
    echo ""
    echo "Commands:"
    echo "  build           Build the Docker image"
    echo "  start           Start the full Coinswap stack"
    echo "  stop            Stop the Coinswap stack"
    echo "  restart         Restart the Coinswap stack"
    echo "  logs [service]  Show logs (optionally for specific service)"
    echo "  status          Show status of running services"
    echo "  shell           Open shell in a new container"
    echo "  test            Run integration tests"
    echo ""
    echo "Individual application commands:"
    echo "  makerd [args]   Run makerd with arguments"
    echo "  maker-cli [args] Run maker-cli with arguments"
    echo "  taker [args]    Run taker with arguments"
    echo "  bitcoin-cli [args] Run bitcoin-cli with arguments"
    echo ""
    echo "Examples:"
    echo "  $0 build"
    echo "  $0 start"
    echo "  $0 taker --help"
    echo "  $0 maker-cli ping"
    echo "  $0 logs makerd"
    echo "  $0 test"
}

# Main script logic
case "${1:-}" in
    "build")
        check_docker
        build_image
        ;;
    "start")
        check_docker
        start_stack
        ;;
    "stop")
        stop_stack
        ;;
    "restart")
        stop_stack
        start_stack
        ;;
    "logs")
        show_logs "$2"
        ;;
    "status")
        cd "$SCRIPT_DIR"
        docker-compose ps
        ;;
    "shell")
        docker run --rm -it --network coinswap-network "$IMAGE_NAME" /bin/sh
        ;;
    "test")
        check_docker
        run_tests
        ;;
    "makerd"|"maker-cli"|"taker"|"directoryd"|"directory-cli"|"bitcoin-cli")
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

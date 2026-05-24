#!/bin/bash
# Debug script to test cargo build in container with more visibility

echo "=== Debugging cargo build in container ==="

# Get the project root directory
PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CARGO_CACHE_DIR="$PROJECT_ROOT/tests/.cargo-cache"
mkdir -p "$CARGO_CACHE_DIR"

# Clean up
docker stop test-cargo-build 2>/dev/null || true
docker rm test-cargo-build 2>/dev/null || true

echo "1. Testing with increased memory and verbose output..."
docker run -it --rm \
    --name test-cargo-build \
    --memory="4g" \
    --cpus="2" \
    -v "$PROJECT_ROOT":/all-smi \
    -v "$CARGO_CACHE_DIR":/usr/local/cargo/registry \
    -w /all-smi \
    rust:1.95 \
    /bin/bash -c "
        echo '=== Container Info ==='
        echo 'Memory limits:'
        cat /sys/fs/cgroup/memory/memory.limit_in_bytes 2>/dev/null || cat /sys/fs/cgroup/memory.max 2>/dev/null || echo 'No memory limit info'
        echo ''
        echo 'CPU info:'
        nproc
        echo ''
        
        echo '=== Installing dependencies ==='
        apt-get update && apt-get install -y pkg-config protobuf-compiler htop
        
        echo ''
        echo '=== Cargo environment ==='
        export CARGO_HTTP_TIMEOUT=600
        export CARGO_NET_RETRY=10
        export CARGO_NET_GIT_FETCH_WITH_CLI=true
        export RUST_BACKTRACE=1
        echo 'CARGO_HTTP_TIMEOUT=600'
        echo 'CARGO_NET_RETRY=10'
        echo 'CARGO_NET_GIT_FETCH_WITH_CLI=true'
        echo 'RUST_BACKTRACE=1'
        
        echo ''
        echo '=== Starting cargo build with verbose output ==='
        echo 'This will show detailed progress...'
        cargo build --release -vv 2>&1 | head -100
        echo ''
        echo 'Exit code:' \$?
    "
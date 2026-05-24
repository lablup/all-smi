#!/bin/bash

echo "Simple container memory test"
echo "============================"
echo ""

# Get the project root directory (parent of tests)
PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CARGO_CACHE_DIR="$PROJECT_ROOT/tests/.cargo-cache"
mkdir -p "$CARGO_CACHE_DIR"

# Run a simple test with build inside container
docker run --rm -it --name all-smi-test-simple-memory \
    --memory="512m" \
    -v "$PROJECT_ROOT":/all-smi \
    -v "$CARGO_CACHE_DIR":/usr/local/cargo/registry \
    -w /all-smi \
    rust:1.95 \
    /bin/bash -c "
        echo 'Installing dependencies...'
        apt-get update -qq && apt-get install -y -qq pkg-config protobuf-compiler >/dev/null 2>&1
        
        echo 'Building all-smi...'
        cargo build --release
        
        echo ''
        echo 'Container cgroup info:'
        cat /proc/self/cgroup
        echo ''
        echo 'Memory files (cgroups v2):'
        ls -la /sys/fs/cgroup/memory.* 2>/dev/null || echo 'No cgroups v2 memory files'
        echo ''
        echo 'Memory files (cgroups v1):'
        ls -la /sys/fs/cgroup/memory/ 2>/dev/null | head -10 || echo 'No cgroups v1 memory files'
        echo ''
        echo 'Running all-smi local to see memory detection:'
        ./target/release/all-smi local 2>&1 | head -20
    "
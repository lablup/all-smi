#!/bin/bash
# Debug script to troubleshoot container startup issues

echo "=== Debug: CPU frequency container test ==="

# Get the project root directory (parent of tests)
PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
echo "Project root: $PROJECT_ROOT"
echo ""

# Clean up
docker stop all-smi-test-debug 2>/dev/null || true
docker rm all-smi-test-debug 2>/dev/null || true

echo "Running container interactively to see what happens..."
echo "Command that will be executed:"
echo "  apt-get update && apt-get install -y pkg-config protobuf-compiler"
echo "  cargo build --release"
echo "  ./target/release/all-smi api --port 9090"
echo ""

docker run -it --rm \
    --name all-smi-test-debug \
    --cpuset-cpus="0,1" \
    -p 9090:9090 \
    -v "$PROJECT_ROOT":/all-smi \
    -w /all-smi \
    rust:1.95 \
    /bin/bash -c "
        echo '=== Starting debug container ==='
        echo 'Current directory:'
        pwd
        echo ''
        echo 'Directory contents:'
        ls -la
        echo ''
        echo 'Checking if Cargo.toml exists:'
        if [ -f Cargo.toml ]; then
            echo 'Cargo.toml found'
        else
            echo 'ERROR: Cargo.toml not found!'
            exit 1
        fi
        echo ''
        echo 'Installing dependencies...'
        apt-get update && apt-get install -y pkg-config protobuf-compiler
        echo ''
        echo 'Building all-smi...'
        cargo build --release
        echo ''
        echo 'Build complete. Starting API server...'
        ./target/release/all-smi api --port 9090
    "
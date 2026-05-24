#!/bin/bash

echo "Quick container API test"
echo "========================"
echo ""

# Get the project root directory (parent of tests)
PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CARGO_CACHE_DIR="$PROJECT_ROOT/tests/.cargo-cache"
mkdir -p "$CARGO_CACHE_DIR"

# Kill any existing container
docker stop all-smi-test-quick-api 2>/dev/null || true
docker rm all-smi-test-quick-api 2>/dev/null || true

# Run container with build
docker run -d --name all-smi-test-quick-api \
    --memory="512m" \
    -v "$PROJECT_ROOT":/all-smi \
    -v "$CARGO_CACHE_DIR":/usr/local/cargo/registry \
    -w /all-smi \
    -p 9999:9999 \
    rust:1.95 \
    /bin/bash -c "
        echo 'Installing dependencies...'
        apt-get update -qq && apt-get install -y -qq pkg-config protobuf-compiler >/dev/null 2>&1
        echo 'Building all-smi...'
        cargo build --release
        echo 'Starting API server...'
        exec ./target/release/all-smi api --port 9999
    "

echo "Waiting for API to start..."
sleep 3

echo ""
echo "Checking stderr logs for debug output:"
docker logs all-smi-test-quick-api 2>&1 | grep DEBUG || echo "No debug output found"

echo ""
echo "Fetching metrics:"
curl -s http://localhost:9999/metrics | grep -E "memory_(total|used|available)" | head -10

echo ""
echo "Full container logs:"
docker logs all-smi-test-quick-api 2>&1

echo ""
echo "Stopping container..."
docker stop all-smi-test-quick-api
#!/bin/bash

echo "Benchmarking container metrics performance"
echo "=========================================="
echo ""

# Get the project root directory (parent of tests)
PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CARGO_CACHE_DIR="$PROJECT_ROOT/tests/.cargo-cache"
mkdir -p "$CARGO_CACHE_DIR"

# Clean up any existing container
docker stop all-smi-test-benchmark-api 2>/dev/null || true
docker rm all-smi-test-benchmark-api 2>/dev/null || true

# Run container with memory limit and build inside
echo "Building and running all-smi in container..."
docker run -d --name all-smi-test-benchmark-api \
    --memory="512m" \
    --cpus="2" \
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
        echo 'Starting API server for benchmarking...'
        exec ./target/release/all-smi api --port 9999
    "

echo "Waiting for API to start..."
sleep 5

# Check if API is responding
if ! curl -s http://localhost:9999/metrics > /dev/null; then
    echo "Error: API is not responding"
    docker logs all-smi-test-benchmark-api
    docker stop all-smi-test-benchmark-api
    exit 1
fi

echo ""
echo "Running benchmark (100 requests)..."
START_TIME=$(date +%s.%N)

for i in {1..100}; do
    curl -s http://localhost:9999/metrics > /dev/null
    if [ $((i % 20)) -eq 0 ]; then
        echo -n "."
    fi
done
echo ""

END_TIME=$(date +%s.%N)
DURATION=$(echo "$END_TIME - $START_TIME" | bc)

echo ""
echo "Benchmark Results:"
echo "=================="
echo "Total requests: 100"
echo "Total time: ${DURATION} seconds"
echo "Requests per second: $(echo "scale=2; 100 / $DURATION" | bc)"
echo "Average response time: $(echo "scale=3; $DURATION / 100 * 1000" | bc) ms"

echo ""
echo "Memory usage of all-smi process:"
docker exec all-smi-test-benchmark-api ps aux | grep all-smi | grep -v grep

echo ""
echo "Sample metrics output (first 20 lines):"
curl -s http://localhost:9999/metrics | head -20

echo ""
echo "Stopping container..."
docker stop all-smi-test-benchmark-api

echo ""
echo "Benchmark complete!"
#!/bin/bash

echo "Testing container-aware metrics in all-smi API mode"
echo "==================================================="
echo ""

# Get the project root directory (parent of tests)
PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CARGO_CACHE_DIR="$PROJECT_ROOT/tests/.cargo-cache"
mkdir -p "$CARGO_CACHE_DIR"

# Build the release binary for baseline test
echo "Building all-smi for baseline test..."
cd "$PROJECT_ROOT" && cargo build --release

echo ""
echo "Test 1: Running outside container (baseline)"
echo "--------------------------------------------"
echo "Starting API mode..."
"$PROJECT_ROOT/target/release/all-smi" api --port 9999 &
API_PID=$!

sleep 3

echo "Fetching metrics..."
curl -s http://localhost:9999/metrics | grep -E "(all_smi_cpu_core_count|all_smi_cpu_utilization|all_smi_memory_total_bytes|all_smi_memory_used_bytes)" | head -10

echo ""
echo "Stopping API server..."
kill $API_PID
wait $API_PID 2>/dev/null

echo ""
echo "Test 2: Running inside Docker container with CPU/Memory limits"
echo "--------------------------------------------------------------"
echo "Note: This requires Docker to be installed and running"
echo ""

# Clean up any existing container
docker stop all-smi-test-metrics-comparison 2>/dev/null || true
docker rm all-smi-test-metrics-comparison 2>/dev/null || true

# Run container with resource limits and build inside
echo "Running container with CPU limit=1.5 and Memory limit=512MB..."
docker run -d --name all-smi-test-metrics-comparison \
    --cpus="1.5" \
    --memory="512m" \
    -v "$PROJECT_ROOT":/all-smi \
    -v "$CARGO_CACHE_DIR":/usr/local/cargo/registry \
    -w /all-smi \
    -p 9999:9999 \
    rust:1.95 \
    /bin/bash -c "
        echo 'Installing dependencies...'
        apt-get update -qq && apt-get install -y -qq pkg-config protobuf-compiler curl >/dev/null 2>&1
        
        echo 'Building all-smi...'
        cargo build --release
        
        echo 'Starting API server...'
        exec ./target/release/all-smi api --port 9999
    "

sleep 5

echo "Fetching metrics from containerized all-smi..."
curl -s http://localhost:9999/metrics | grep -E "(all_smi_cpu_core_count|all_smi_cpu_utilization|all_smi_memory_total_bytes|all_smi_memory_used_bytes)" | head -10

echo ""
echo "Container runtime info:"
curl -s http://localhost:9999/metrics | grep "all_smi_container_runtime_info"

echo ""
echo "Stopping container..."
docker stop all-smi-test-metrics-comparison

echo ""
echo "Test complete!"
echo ""
echo "Expected results:"
echo "- Outside container: Shows full system CPU cores and memory"
echo "- Inside container: Shows limited CPU cores (1-2) and memory (512MB)"
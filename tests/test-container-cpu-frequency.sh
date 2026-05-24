#!/bin/bash
# Script to test CPU frequency monitoring in container

echo "=== Testing CPU frequency monitoring in container ==="

# Get the project root directory (parent of tests)
PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
echo "Project root: $PROJECT_ROOT"
echo ""

# Function to wait for container to be ready
wait_for_container() {
    local container_name=$1
    local port=$2
    local max_attempts=60  # 5 minutes max wait
    local attempt=0
    
    echo "   Waiting for $container_name to be ready..."
    
    while [ $attempt -lt $max_attempts ]; do
        # Check if container is still running
        if ! docker ps | grep -q "$container_name"; then
            echo "   ERROR: Container $container_name is not running!"
            docker logs "$container_name" 2>&1 | tail -20
            return 1
        fi
        
        # Check if API is responding
        if curl -s -f "http://localhost:$port/metrics" >/dev/null 2>&1; then
            echo "   Container $container_name is ready!"
            return 0
        fi
        
        # Show build progress
        if [ $((attempt % 10)) -eq 0 ]; then
            echo "   Still waiting... checking logs:"
            docker logs "$container_name" 2>&1 | grep -E "Building|Compiling|Starting" | tail -5
        fi
        
        sleep 5
        ((attempt++))
    done
    
    echo "   ERROR: Timeout waiting for $container_name"
    return 1
}

# Clean up any existing containers first
echo "0. Cleaning up any existing test containers..."
docker stop all-smi-test-cpu-freq-cpuset all-smi-test-cpu-freq-quota 2>/dev/null || true
docker rm all-smi-test-cpu-freq-cpuset all-smi-test-cpu-freq-quota 2>/dev/null || true

# Create local cargo cache directory for faster rebuilds
CARGO_CACHE_DIR="$PROJECT_ROOT/tests/.cargo-cache"
mkdir -p "$CARGO_CACHE_DIR"
echo "Using cargo cache at: $CARGO_CACHE_DIR"
echo "Note: First build will be slower as it downloads dependencies"
echo ""

# Run container with cpuset limitation
echo "1. Running container with cpuset limitation (CPU 0,1)..."
docker run -d --name all-smi-test-cpu-freq-cpuset \
    --cpuset-cpus="0,1" \
    --memory="2g" \
    --memory-swap="2g" \
    -p 9090:9090 \
    -v "$PROJECT_ROOT":/all-smi \
    -v "$CARGO_CACHE_DIR":/usr/local/cargo/registry \
    -w /all-smi \
    rust:1.95 \
    /bin/bash -c "
        echo '[Container] Starting at: ' \$(date)
        echo '[Container] Container ID: ' \$(hostname)
        echo '[Container] Working directory: ' \$(pwd)
        echo '[Container] Checking if project is mounted...'
        ls -la Cargo.toml 2>&1 || echo 'ERROR: Cargo.toml not found!'
        
        echo '[Container] Installing dependencies...'
        apt-get update -qq && apt-get install -y -qq pkg-config protobuf-compiler curl || {
            echo '[Container] ERROR: Failed to install dependencies'
            exit 1
        }
        echo '[Container] Dependencies installed successfully'
        
        echo '[Container] Rust/Cargo versions:'
        rustc --version
        cargo --version
        
        echo '[Container] Building all-smi in container...'
        echo '[Container] Setting cargo timeout and retry settings...'
        export CARGO_HTTP_TIMEOUT=300
        export CARGO_NET_RETRY=3
        export RUST_LOG=all_smi=debug
        
        echo '[Container] Running: cargo build --release'
        echo '[Container] This may take several minutes on first run...'
        cargo build --release 2>&1 || {
            echo '[Container] ERROR: Build failed!'
            echo '[Container] Exit code: ' \$?
            exit 1
        }
        echo '[Container] Build completed successfully'
        
        echo '[Container] Checking if binary exists...'
        ls -la ./target/release/all-smi || {
            echo '[Container] ERROR: Binary not found after build!'
            exit 1
        }
        
        echo '[Container] Starting API server with cpuset limitation...'
        exec ./target/release/all-smi api --port 9090 2>&1
    "

# Run container with CPU quota limitation (2 CPUs worth)
echo "2. Running container with CPU quota limitation..."
docker run -d --name all-smi-test-cpu-freq-quota \
    --cpu-quota=200000 --cpu-period=100000 \
    --memory="2g" \
    --memory-swap="2g" \
    -p 9091:9091 \
    -v "$PROJECT_ROOT":/all-smi \
    -v "$CARGO_CACHE_DIR":/usr/local/cargo/registry \
    -w /all-smi \
    rust:1.95 \
    /bin/bash -c "
        echo '[Container] Starting at: ' \$(date)
        echo '[Container] Container ID: ' \$(hostname)
        echo '[Container] Working directory: ' \$(pwd)
        echo '[Container] Checking if project is mounted...'
        ls -la Cargo.toml 2>&1 || echo 'ERROR: Cargo.toml not found!'
        
        echo '[Container] Installing dependencies...'
        apt-get update -qq && apt-get install -y -qq pkg-config protobuf-compiler curl || {
            echo '[Container] ERROR: Failed to install dependencies'
            exit 1
        }
        echo '[Container] Dependencies installed successfully'
        
        echo '[Container] Rust/Cargo versions:'
        rustc --version
        cargo --version
        
        echo '[Container] Building all-smi in container...'
        echo '[Container] Setting cargo timeout and retry settings...'
        export CARGO_HTTP_TIMEOUT=300
        export CARGO_NET_RETRY=3
        export RUST_LOG=all_smi=debug
        
        echo '[Container] Running: cargo build --release'
        echo '[Container] This may take several minutes on first run...'
        cargo build --release 2>&1 || {
            echo '[Container] ERROR: Build failed!'
            echo '[Container] Exit code: ' \$?
            exit 1
        }
        echo '[Container] Build completed successfully'
        
        echo '[Container] Checking if binary exists...'
        ls -la ./target/release/all-smi || {
            echo '[Container] ERROR: Binary not found after build!'
            exit 1
        }
        
        echo '[Container] Starting API server with CPU quota limitation...'
        exec ./target/release/all-smi api --port 9091 2>&1
    "

# Start monitoring containers in the background
echo "3. Starting container monitoring..."
(
    sleep 5
    echo ""
    echo "=== Container status check ==="
    docker ps -a | grep all-smi-test-cpu-freq || echo "No test containers running"
    
    # Check logs regardless of status
    echo ""
    echo "=== Cpuset container logs (first 5 seconds) ==="
    docker logs all-smi-test-cpu-freq-cpuset 2>&1 || echo "No logs available"
    
    echo ""
    echo "=== Quota container logs (first 5 seconds) ==="
    docker logs all-smi-test-cpu-freq-quota 2>&1 || echo "No logs available"
) &

# Wait for containers to be ready
echo "4. Waiting for containers to build and start..."
wait_for_container "all-smi-test-cpu-freq-cpuset" 9090
cpuset_ready=$?
wait_for_container "all-smi-test-cpu-freq-quota" 9091
quota_ready=$?

if [ $cpuset_ready -ne 0 ] && [ $quota_ready -ne 0 ]; then
    echo "ERROR: Both containers failed to start properly"
    exit 1
fi

# Check container logs for debug output
echo "4. Checking container logs for CPU frequency detection..."
if [ $cpuset_ready -eq 0 ]; then
    echo "=== Container logs (cpuset mode) ==="
    docker logs all-smi-test-cpu-freq-cpuset 2>&1 | grep -E "Container CPU frequency|frequency|MHz|cpuset" | head -20
    echo ""
    echo "=== Full API startup logs (cpuset mode) ==="
    docker logs all-smi-test-cpu-freq-cpuset 2>&1 | tail -20
fi

echo ""
if [ $quota_ready -eq 0 ]; then
    echo "=== Container logs (quota mode) ==="
    docker logs all-smi-test-cpu-freq-quota 2>&1 | grep -E "Container CPU frequency|cpu|frequency|MHz" | head -10
fi

# Check metrics
echo ""
echo "5. Checking CPU frequency metrics..."
if [ $cpuset_ready -eq 0 ]; then
    echo "=== CPU Frequency Metrics (cpuset mode) ==="
    curl -s http://localhost:9090/metrics | grep -E "all_smi_cpu_frequency_mhz|all_smi_cpu_socket_frequency_mhz" || echo "No frequency metrics found"
    
    echo ""
    echo "=== Sample CPU metrics (cpuset mode) ==="
    curl -s http://localhost:9090/metrics | grep "all_smi_cpu_" | head -10
fi

echo ""
if [ $quota_ready -eq 0 ]; then
    echo "=== CPU Frequency Metrics (quota mode) ==="
    curl -s http://localhost:9091/metrics | grep -E "all_smi_cpu_frequency_mhz|all_smi_cpu_socket_frequency_mhz" || echo "No frequency metrics found"
fi

# Show raw CPU info in container for debugging
echo ""
echo "6. Debugging: CPU info in containers..."
if [ $cpuset_ready -eq 0 ]; then
    echo "=== CPU info in cpuset container ==="
    docker exec all-smi-test-cpu-freq-cpuset bash -c "grep -E 'processor|cpu MHz|model name' /proc/cpuinfo | head -20"
    
    echo ""
    echo "=== Cgroup info in cpuset container ==="
    docker exec all-smi-test-cpu-freq-cpuset bash -c "cat /sys/fs/cgroup/cpuset/cpuset.cpus 2>/dev/null || cat /sys/fs/cgroup/cpuset.cpus 2>/dev/null || echo 'cpuset info not available'"
fi

# Compare with host CPU frequency
echo ""
echo "7. Host CPU frequency for comparison:"
grep "cpu MHz" /proc/cpuinfo | head -5

# Cleanup
echo ""
echo "8. Cleaning up..."
docker stop all-smi-test-cpu-freq-cpuset all-smi-test-cpu-freq-quota 2>/dev/null || true
docker rm all-smi-test-cpu-freq-cpuset all-smi-test-cpu-freq-quota 2>/dev/null || true

echo ""
echo "=== Test complete ==="
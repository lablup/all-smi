#!/bin/bash
# Enhanced test script for ARM CPU frequency detection

echo "=== Testing ARM CPU frequency detection ==="

# Get the project root directory
PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CARGO_CACHE_DIR="$PROJECT_ROOT/tests/.cargo-cache"
mkdir -p "$CARGO_CACHE_DIR"

# Clean up any existing containers
docker stop all-smi-test-arm-freq-detection 2>/dev/null || true
docker rm all-smi-test-arm-freq-detection 2>/dev/null || true

echo "1. Checking ARM CPU frequency sources in container..."
docker run --rm \
    --name all-smi-test-arm-freq-check \
    -v "$PROJECT_ROOT":/all-smi \
    ubuntu:22.04 \
    /bin/bash -c "
        echo '=== Checking various ARM frequency sources ==='
        echo ''
        echo '1. /proc/cpuinfo frequency fields:'
        grep -i 'mhz\|freq\|bogomips' /proc/cpuinfo | head -10 || echo '   No frequency fields in cpuinfo'
        
        echo ''
        echo '2. cpufreq sysfs (standard paths):'
        for cpu in 0 1; do
            echo "   CPU\$cpu:"
            cat /sys/devices/system/cpu/cpu\$cpu/cpufreq/cpuinfo_max_freq 2>/dev/null && echo ' (cpuinfo_max_freq)' || echo -n ''
            cat /sys/devices/system/cpu/cpu\$cpu/cpufreq/scaling_cur_freq 2>/dev/null && echo ' (scaling_cur_freq)' || echo -n ''
        done
        
        echo ''
        echo '3. cpufreq policy paths:'
        ls -la /sys/devices/system/cpu/cpufreq/ 2>/dev/null || echo '   No cpufreq directory'
        for policy in /sys/devices/system/cpu/cpufreq/policy*; do
            if [ -d \"\$policy\" ]; then
                echo \"   \$policy:\"
                cat \$policy/cpuinfo_max_freq 2>/dev/null && echo ' (max)' || echo -n ''
                cat \$policy/scaling_cur_freq 2>/dev/null && echo ' (cur)' || echo -n ''
            fi
        done
        
        echo ''
        echo '4. ARM-specific paths:'
        cat /sys/devices/system/cpu/cpu0/clock_rate 2>/dev/null && echo ' (/sys/.../cpu0/clock_rate)' || echo '   No clock_rate file'
        
        echo ''
        echo '5. Device tree:'
        if [ -f /proc/device-tree/cpus/cpu@0/clock-frequency ]; then
            echo -n '   Found device-tree clock-frequency: '
            od -An -tx4 /proc/device-tree/cpus/cpu@0/clock-frequency 2>/dev/null || echo 'unable to read'
        else
            echo '   No device-tree clock-frequency'
        fi
        
        echo ''
        echo '6. lscpu output:'
        apt-get update -qq && apt-get install -y -qq util-linux >/dev/null 2>&1
        lscpu | grep -i 'mhz\|freq' || echo '   No frequency info in lscpu'
        
        echo ''
        echo '7. BogoMIPS values:'
        grep -i bogomips /proc/cpuinfo | head -5
    "

echo ""
echo "2. Building and running all-smi with enhanced ARM frequency detection..."
docker run -d --name all-smi-test-arm-freq-detection \
    --cpuset-cpus="0,1" \
    --memory="2g" \
    -p 9095:9095 \
    -v "$PROJECT_ROOT":/all-smi \
    -v "$CARGO_CACHE_DIR":/usr/local/cargo/registry \
    -w /all-smi \
    rust:1.95 \
    /bin/bash -c "
        echo '[Container] Installing dependencies...'
        apt-get update -qq && apt-get install -y -qq pkg-config protobuf-compiler >/dev/null 2>&1
        
        echo '[Container] Building with ARM frequency detection...'
        export RUST_LOG=all_smi=debug
        export RUST_BACKTRACE=1
        cargo build --release 2>&1
        
        echo '[Container] Starting API server...'
        ./target/release/all-smi api --port 9095 2>&1
    "

echo "3. Waiting for API to start..."
sleep 30

# Check if container is still running
if ! docker ps | grep -q all-smi-test-arm-freq-detection; then
    echo "ERROR: Container exited!"
    echo "Container logs:"
    docker logs all-smi-test-arm-freq-detection 2>&1 | tail -50
    docker rm all-smi-test-arm-freq-detection
    exit 1
fi

echo "4. Checking frequency detection logs..."
echo "=== ARM frequency detection messages ==="
docker logs all-smi-test-arm-freq-detection 2>&1 | grep -i "arm cpu frequency" || echo "No ARM frequency debug messages found"

echo ""
echo "5. Checking API metrics..."
echo "=== CPU frequency metrics ==="
curl -s http://localhost:9095/metrics | grep -E "all_smi_cpu_frequency_mhz|all_smi_cpu_socket_frequency_mhz" | head -10

echo ""
echo "6. Full CPU info from metrics..."
echo "=== CPU model and info ==="
curl -s http://localhost:9095/metrics | grep "all_smi_cpu" | grep -E "model|frequency" | head -10

# Cleanup
echo ""
echo "7. Cleaning up..."
docker stop all-smi-test-arm-freq-detection 2>/dev/null || true
docker rm all-smi-test-arm-freq-detection 2>/dev/null || true

echo ""
echo "=== Test complete ==="
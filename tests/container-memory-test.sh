#!/bin/bash

echo "Testing container memory detection"
echo "=================================="
echo ""

# Get the project root directory (parent of tests)
PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CARGO_CACHE_DIR="$PROJECT_ROOT/tests/.cargo-cache"
mkdir -p "$CARGO_CACHE_DIR"

# Clean up any existing container
docker stop all-smi-test-memory-allocation 2>/dev/null || true
docker rm all-smi-test-memory-allocation 2>/dev/null || true

# Create memory test program
cat > /tmp/memory-eater.c << 'EOF'
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

int main() {
    printf("Allocating 100MB of memory...\n");
    
    size_t size = 100 * 1024 * 1024;
    char *buffer = malloc(size);
    
    if (buffer == NULL) {
        printf("Failed to allocate memory\n");
        return 1;
    }
    
    memset(buffer, 'A', size);
    printf("Memory allocated and written. Sleeping for 30 seconds...\n");
    
    sleep(30);
    
    free(buffer);
    printf("Memory freed.\n");
    return 0;
}
EOF

# Run container with memory limit and build all-smi inside
echo "Starting container with 512MB memory limit..."
docker run -d --name all-smi-test-memory-allocation \
    --memory="512m" \
    -v "$PROJECT_ROOT":/all-smi \
    -v "$CARGO_CACHE_DIR":/usr/local/cargo/registry \
    -v /tmp/memory-eater.c:/tmp/memory-eater.c \
    -w /all-smi \
    -p 9999:9999 \
    rust:1.95 \
    /bin/bash -c "
        echo 'Installing dependencies...'
        apt-get update -qq && apt-get install -y -qq pkg-config protobuf-compiler gcc curl >/dev/null 2>&1
        
        echo 'Compiling memory test program...'
        gcc -o /tmp/memory-eater /tmp/memory-eater.c
        
        echo 'Building all-smi...'
        cargo build --release
        
        echo 'Starting all-smi API...'
        ./target/release/all-smi api --port 9999 &
        API_PID=\$!
        
        sleep 5
        
        echo 'Starting memory allocation test...'
        /tmp/memory-eater &
        EATER_PID=\$!
        
        wait \$EATER_PID
        tail -f /dev/null
    "

echo ""
echo "Waiting for container to start..."
sleep 8

echo ""
echo "Initial memory usage (before allocation):"
curl -s http://localhost:9999/metrics | grep -E "all_smi_memory_(total|used|available)_bytes" | head -5

echo ""
echo "Waiting for memory allocation..."
sleep 10

echo ""
echo "Memory usage after allocating 100MB:"
curl -s http://localhost:9999/metrics | grep -E "all_smi_memory_(total|used|available)_bytes" | head -5

echo ""
echo "Container runtime info:"
curl -s http://localhost:9999/metrics | grep "all_smi_container_runtime_info"

echo ""
echo "Stopping container..."
docker stop all-smi-test-memory-allocation

# Cleanup
rm -f /tmp/memory-eater.c

echo ""
echo "Test complete!"
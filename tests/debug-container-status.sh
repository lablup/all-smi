#!/bin/bash
# Script to debug container lifecycle

echo "=== Debugging container lifecycle ==="

# Get the project root directory
PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# Clean up
docker stop test-lifecycle 2>/dev/null || true
docker rm test-lifecycle 2>/dev/null || true

echo "1. Starting a simple test container..."
docker run -d --name test-lifecycle \
    -v "$PROJECT_ROOT":/all-smi \
    -w /all-smi \
    rust:1.95 \
    /bin/bash -c "
        echo 'Container started successfully'
        echo 'Sleeping for 10 seconds...'
        sleep 10
        echo 'Container finishing normally'
    "

echo "2. Checking container status immediately..."
docker ps | grep test-lifecycle || echo "Container not in 'docker ps'"
docker ps -a | grep test-lifecycle || echo "Container not in 'docker ps -a'"

echo ""
echo "3. Waiting 2 seconds and checking again..."
sleep 2
docker ps | grep test-lifecycle || echo "Container not running"
docker ps -a | grep test-lifecycle || echo "Container not found at all"

echo ""
echo "4. Container logs:"
docker logs test-lifecycle 2>&1

echo ""
echo "5. Container exit code:"
docker inspect test-lifecycle --format='{{.State.ExitCode}}' 2>/dev/null || echo "Cannot get exit code"

# Now test with cargo build
echo ""
echo "=== Testing with cargo build ==="
docker stop test-build 2>/dev/null || true
docker rm test-build 2>/dev/null || true

echo "6. Starting container with cargo build..."
docker run -d --name test-build \
    -v "$PROJECT_ROOT":/all-smi \
    -w /all-smi \
    rust:1.95 \
    /bin/bash -c "
        echo 'Starting cargo build test'
        echo 'Checking environment:'
        echo '  PWD:' \$(pwd)
        echo '  Cargo.toml exists:' \$(test -f Cargo.toml && echo 'yes' || echo 'no')
        echo '  Rust version:' \$(rustc --version)
        echo ''
        echo 'Running cargo build --release...'
        timeout 300 cargo build --release 2>&1 || echo 'Cargo build failed or timed out'
        echo 'Build command finished'
    "

echo "7. Monitoring build container..."
for i in {1..10}; do
    sleep 3
    if docker ps | grep -q test-build; then
        echo "  After ${i}0 seconds: Container still running"
    else
        echo "  After ${i}0 seconds: Container stopped"
        echo "  Exit code: $(docker inspect test-build --format='{{.State.ExitCode}}' 2>/dev/null || echo 'unknown')"
        echo "  Last logs:"
        docker logs --tail 20 test-build 2>&1
        break
    fi
done

# Cleanup
echo ""
echo "8. Cleaning up..."
docker stop test-lifecycle test-build 2>/dev/null || true
docker rm test-lifecycle test-build 2>/dev/null || true

echo "=== Debug complete ==="
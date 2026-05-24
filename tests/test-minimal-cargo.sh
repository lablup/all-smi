#!/bin/bash
# Minimal test to check if cargo update works

echo "=== Testing minimal cargo operations ==="

# Get the project root directory
PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# Clean up
docker stop test-minimal 2>/dev/null || true
docker rm test-minimal 2>/dev/null || true

echo "1. Testing basic cargo command..."
docker run --rm \
    --name test-minimal \
    -v "$PROJECT_ROOT":/all-smi \
    -w /all-smi \
    rust:1.95 \
    /bin/bash -c "
        echo 'Testing cargo version:'
        cargo --version
        echo ''
        echo 'Checking Cargo.toml:'
        head -10 Cargo.toml
        echo ''
        echo 'Testing cargo metadata (fast operation):'
        timeout 30 cargo metadata --format-version 1 --no-deps > /dev/null 2>&1
        echo 'Cargo metadata exit code:' \$?
        echo ''
        echo 'Testing cargo update with timeout:'
        timeout 60 cargo update --dry-run 2>&1 | head -20
        echo 'Cargo update exit code:' \$?
    "

echo ""
echo "2. Testing with strace to see what happens..."
docker run --rm \
    --cap-add SYS_PTRACE \
    --name test-strace \
    -v "$PROJECT_ROOT":/all-smi \
    -w /all-smi \
    rust:1.95 \
    /bin/bash -c "
        apt-get update -qq && apt-get install -y -qq strace
        echo 'Running cargo update with strace (last 50 lines):'
        timeout 30 strace -f -e trace=network,signal cargo update --dry-run 2>&1 | tail -50
    "
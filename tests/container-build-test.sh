#!/bin/bash

echo "Testing all-smi build inside container"
echo "======================================"
echo ""

# Get the project root directory (parent of tests)
PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# Create test script that will run inside container
cat > /tmp/run-build-test.sh << 'EOF'
#!/bin/bash

echo "Building all-smi inside container..."
cd /all-smi
cargo build --release

if [ $? -ne 0 ]; then
    echo "Build failed!"
    exit 1
fi

echo ""
echo "Build successful! Running API test..."
/all-smi/target/release/all-smi api --port 9999 &
API_PID=$!

sleep 5

echo ""
echo "Testing metrics endpoint..."
curl -s http://localhost:9999/metrics | grep -E "(all_smi_cpu|all_smi_memory)" | head -10

kill $API_PID 2>/dev/null

echo ""
echo "Test completed!"
EOF

chmod +x /tmp/run-build-test.sh

echo "Running build test in Docker container..."
echo ""

docker run --rm \
    --name all-smi-build-test \
    --memory="4g" \
    --cpus="4" \
    -v "$PROJECT_ROOT":/all-smi \
    -v "/tmp/run-build-test.sh":/tmp/run-build-test.sh \
    -w /all-smi \
    rust:1.95 \
    /bin/bash -c "
        apt-get update && apt-get install -y pkg-config protobuf-compiler curl && 
        /tmp/run-build-test.sh
    "

# Cleanup
rm -f /tmp/run-build-test.sh

echo ""
echo "Build test completed!"
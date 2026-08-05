.PHONY: help all container-cpu-frequency arm-cpu-frequency container-benchmark container-cpu container-memory container-simple quick-api homebrew-formula-workflow clean

# Default target
help:
	@echo "all-smi Container Tests"
	@echo ""
	@echo "Available test targets:"
	@echo ""
	@echo "Container Resource Tests:"
	@echo "  container-cpu-frequency      Test CPU frequency detection (builds in container)"
	@echo "  arm-cpu-frequency           Test ARM CPU frequency detection specifically"
	@echo "  container-cpu               Test CPU limit detection (1.5 CPUs)"
	@echo "  container-memory            Test memory limit detection (complex)"
	@echo "  container-simple            Simple container test with memory limits"
	@echo "  container-benchmark         Benchmark API performance in container"
	@echo "  quick-api                   Quick API functionality test"
	@echo ""
	@echo "Integration Tests:"
	@echo "  remote-monitoring           Test remote monitoring with multiple mock servers (TODO)"
	@echo "  prometheus-metrics          Test Prometheus metrics export format (TODO)"
	@echo ""
	@echo "Performance Tests:"
	@echo "  high-scale-monitoring       Test monitoring 100+ remote nodes (TODO)"
	@echo "  api-load-test              Test API performance under load (TODO)"
	@echo ""
	@echo "Workflow Tests:"
	@echo "  homebrew-formula-workflow   Run the Homebrew tap workflow's step bodies (macOS only)"
	@echo ""
	@echo "Utility:"
	@echo "  all                        Run all available tests"
	@echo "  clean                      Clean up test artifacts and containers"

# Run all tests
all: container-cpu-frequency arm-cpu-frequency

# Test CPU frequency detection in containers (builds in container)
container-cpu-frequency:
	@echo "=== Running CPU frequency container test (with build) ==="
	@./test-container-cpu-frequency.sh

# Test ARM CPU frequency detection specifically
arm-cpu-frequency:
	@echo "=== Running ARM CPU frequency detection test ==="
	@./test-arm-cpu-frequency.sh

# Test CPU limits
container-cpu:
	@echo "=== Running container CPU test ==="
	@./container-cpu-test.sh

# Test memory detection
container-memory:
	@echo "=== Running container memory test ==="
	@./container-memory-test.sh

# Simple container test
container-simple:
	@echo "=== Running simple container test ==="
	@./container-simple-test.sh

# Benchmark test
container-benchmark:
	@echo "=== Running container benchmark ==="
	@./container-benchmark.sh

# Quick API test
quick-api:
	@echo "=== Running quick API test ==="
	@./quick-api-test.sh

# Test memory limit detection in containers (TODO)
container-memory-limits:
	@echo "=== Memory limit test not yet implemented ==="
	@echo "TODO: Create test-container-memory-limits.sh"

# Test GPU detection in containers (TODO)
container-gpu-detection:
	@echo "=== GPU detection test not yet implemented ==="
	@echo "TODO: Create test-container-gpu-detection.sh"

# Test remote monitoring with multiple mock servers (TODO)
remote-monitoring:
	@echo "=== Remote monitoring test not yet implemented ==="
	@echo "TODO: Create test-remote-monitoring.sh"

# Test Prometheus metrics format (TODO)
prometheus-metrics:
	@echo "=== Prometheus metrics test not yet implemented ==="
	@echo "TODO: Create test-prometheus-metrics.sh"

# Test high-scale monitoring (TODO)
high-scale-monitoring:
	@echo "=== High-scale monitoring test not yet implemented ==="
	@echo "TODO: Create test-high-scale-monitoring.sh"

# API load test (TODO)
api-load-test:
	@echo "=== API load test not yet implemented ==="
	@echo "TODO: Create test-api-load.sh"

# Run the update_homebrew_formula.yml step bodies against fixtures. Offline, and
# deliberately not part of `all`: the bodies call gsed, brew style and BSD awk,
# so this needs macOS, matching the workflow's own runs-on.
homebrew-formula-workflow:
	@echo "=== Running Homebrew formula workflow step tests ==="
	@./homebrew-formula-workflow/run-workflow-steps.sh

# Clean up test artifacts
clean:
	@echo "=== Cleaning up test containers and artifacts ==="
	@docker ps -a | grep "all-smi-test" | awk '{print $$1}' | xargs -r docker stop 2>/dev/null || true
	@docker ps -a | grep "all-smi-test" | awk '{print $$1}' | xargs -r docker rm 2>/dev/null || true
	@echo "Cleanup complete"
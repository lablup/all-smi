.PHONY: help local remote mock release test lint clean docker-dev docker-test-container-api docker-test-container-view

help:
	@echo "all-smi"
	@echo ""
	@echo "Available targets:"
	@echo ""
	@echo "Setup & Building:"
	@echo "  local                        Run for local view mode"
	@echo "  remote                       Run for remote view mode"
	@echo "  api                          Run for API mode"
	@echo "  mock                         Run mock server for testing"
	@echo "  docker-dev                   Run container dev env with bash"
	@echo "  docker-test-container-api    Test container API mode"
	@echo "  docker-test-container-view   Test container view mode"
	@echo ""
	@echo "Quality & Testing:"
	@echo "  test                         Run tests"
	@echo "  validate                     Validate links and content"
	@echo "  lint                         Run linting on documentation"
	@echo "  test                         Run all tests"
	@echo ""
	@echo "Deployment:"
	@echo "  release                      Build release binaries"
	@echo "  clean                        Clean build artifacts"

local:
	cargo run --bin all-smi -- local

api:
	cargo run --bin all-smi -- api

remote:
	SUPPRESS_LOCALHOST_WARNING=1 cargo run --bin all-smi -- view --hostfile ./hosts.csv

mock:
	cargo run --features mock --bin all-smi-mock-server -- --port-range 10001-10050

docker-dev:
	@mkdir -p tests/.cargo-cache
	docker run -it --rm --name all-smi-dev-container --memory="4g" --cpus="2.5" \
		-v "$(PWD)":/all-smi \
		-v "$(PWD)/tests/.cargo-cache":/usr/local/cargo/registry \
		-w /all-smi \
		rust:1.95 \
		/bin/bash -c "apt-get update && apt-get install -y pkg-config protobuf-compiler && \
		bash"

docker-test-container-api:
	@mkdir -p tests/.cargo-cache
	docker run -it --rm --memory="2g" --cpus="1.5" \
		-p 9090:9090 \
		-v "$(PWD)":/all-smi \
		-v "$(PWD)/tests/.cargo-cache":/usr/local/cargo/registry \
		-w /all-smi \
		rust:1.95 \
		/bin/bash -c "apt-get update && apt-get install -y pkg-config protobuf-compiler && \
		cargo build --release && \
		./target/release/all-smi api --port 9090"

docker-test-container-view:
	@mkdir -p tests/.cargo-cache
	docker run -it --rm --memory="2g" --cpus="1.5" \
		-v "$(PWD)":/all-smi \
		-v "$(PWD)/tests/.cargo-cache":/usr/local/cargo/registry \
		-w /all-smi \
		rust:1.95 \
		/bin/bash -c "apt-get update && apt-get install -y pkg-config protobuf-compiler && \
		cargo build --release && \
		./target/release/all-smi local"

docker-build-container:
	@mkdir -p tests/.cargo-cache
	docker build -t all-smi:latest .

release:
	cargo build --release

test:
	cargo test --all

lint:
	cargo fmt --features=all -- --check
	cargo clippy --features=all -- -D warnings

clean:
	cargo clean
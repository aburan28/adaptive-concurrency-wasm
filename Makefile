.PHONY: build test clean docker-up docker-down fmt lint load-test e2e-test

WASM_TARGET = wasm32-unknown-unknown
WASM_OUT = target/$(WASM_TARGET)/release/adaptive_concurrency_wasm.wasm

# Ensure rustup's toolchain is used (Homebrew rustc may shadow it and lacks wasm32 sysroot).
TOOLCHAIN_BIN = $(HOME)/.rustup/toolchains/stable-aarch64-apple-darwin/bin
export PATH := $(TOOLCHAIN_BIN):$(PATH)

build:
	cargo build --target $(WASM_TARGET) --release
	@ls -lh $(WASM_OUT)
	@echo "WASM binary built: $(WASM_OUT)"

# Run unit tests on native host target
test:
	cargo test

clean:
	cargo clean

fmt:
	cargo fmt

lint:
	cargo clippy --target $(WASM_TARGET) -- -D warnings

docker-up: build
	docker compose up --build -d

docker-down:
	docker compose down

docker-logs:
	docker compose logs -f envoy

# Run comprehensive e2e tests (requires docker)
e2e-test: build
	docker compose -f docker-compose.e2e.yaml up -d --wait
	python3 tests/e2e/run_tests.py; ret=$$?; \
	docker compose -f docker-compose.e2e.yaml down; \
	exit $$ret

# Send test traffic through Envoy
load-test:
	@echo "Sending 500 requests through Envoy..."
	@for i in $$(seq 1 500); do \
		curl -s -o /dev/null -w "req=$$i status=%{http_code} time=%{time_total}s server=%{header.x-server-name}\n" \
			http://localhost:10000/ & \
		if [ $$((i % 20)) -eq 0 ]; then wait; fi; \
	done
	@wait
	@echo "Done."

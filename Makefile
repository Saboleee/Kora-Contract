# =============================================================================
# Kora Protocol — Makefile
# =============================================================================

.PHONY: build test clean fmt lint check audit deploy-testnet deploy-mainnet

WASM_TARGET := wasm32-unknown-unknown
CONTRACTS   := access_control invoice_nft marketplace financing_pool treasury risk_registry

# ── Build ─────────────────────────────────────────────────────────────────────

build:
	cargo build --target $(WASM_TARGET) --release

build-optimized: build
	@for c in $(CONTRACTS); do \
		wasm="target/$(WASM_TARGET)/release/kora_$${c}.wasm"; \
		if [ -f "$$wasm" ]; then \
			stellar contract optimize --wasm "$$wasm"; \
			echo "Optimized: $$wasm"; \
		fi; \
	done

# ── Test ──────────────────────────────────────────────────────────────────────

test:
	cargo test --all

test-verbose:
	cargo test --all -- --nocapture

# ── Code Quality ──────────────────────────────────────────────────────────────

fmt:
	cargo fmt --all

lint:
	cargo clippy --all -- -D warnings

check:
	cargo check --all

# ── Audit ─────────────────────────────────────────────────────────────────────

audit:
	cargo audit
	cargo deny check

# ── Clean ─────────────────────────────────────────────────────────────────────

clean:
	cargo clean

# ── Deploy ────────────────────────────────────────────────────────────────────

deploy-testnet: build-optimized
	bash scripts/deploy.sh testnet

deploy-mainnet: build-optimized
	@echo "WARNING: Deploying to MAINNET. Press Ctrl+C to abort, Enter to continue."
	@read _
	bash scripts/deploy.sh mainnet

# ── Helpers ───────────────────────────────────────────────────────────────────

setup:
	rustup target add $(WASM_TARGET)
	cargo install stellar-cli --locked

sizes: build
	@echo "WASM sizes:"
	@for c in $(CONTRACTS); do \
		wasm="target/$(WASM_TARGET)/release/kora_$${c}.wasm"; \
		if [ -f "$$wasm" ]; then \
			printf "  %-25s %s\n" "$$c" "$$(du -sh $$wasm | cut -f1)"; \
		fi; \
	done

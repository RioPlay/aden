INSTALL_DIR := $(HOME)/.local/bin
BINARY      := aden

.PHONY: build test install dev release ci

# Fast dev build + install. Use this during active development.
dev:
	cargo build -p aden-cli
	@cargo test --workspace --quiet 2>&1 | tail -5
	cp target/debug/$(BINARY) $(INSTALL_DIR)/$(BINARY)
	@echo "Installed $(INSTALL_DIR)/$(BINARY) (debug)"

# Release build + install. Use before sharing or benchmarking.
release:
	cargo build -p aden-cli --release
	@cargo test --workspace --quiet 2>&1 | tail -5
	cp target/release/$(BINARY) $(INSTALL_DIR)/$(BINARY)
	@echo "Installed $(INSTALL_DIR)/$(BINARY) (release)"

# Run tests only (no install).
test:
	cargo test --workspace

# Build only (no install, no test).
build:
	cargo build --workspace

# Full CI gates (check + heal + test).
ci:
	cargo build --workspace
	cargo test --workspace
	aden check .

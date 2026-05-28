INSTALL_SCRIPT := ./install.sh

.PHONY: build test install dev release ci

# Fast dev build + install. Use this during active development.
dev:
	cargo build -p aden-cli
	@cargo test --workspace --quiet 2>&1 | tail -5
	$(INSTALL_SCRIPT)
	@echo "Installed (debug)"

# Release build + install. Use before sharing or benchmarking.
release:
	INSTALL_WAS_DONE=1 $(INSTALL_SCRIPT)
	@echo "Installed (release)"

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

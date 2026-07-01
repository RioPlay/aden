INSTALL_SCRIPT := ./install.sh

.PHONY: build test install dev release ci lsp

# aden-lsp is a standalone, unshipped binary (install.sh never builds it). The
# default targets exclude it to skip the heavy tower-lsp stack; `make lsp` builds
# it on demand.
dev:
	cargo build -p aden-cli
	@cargo test --workspace --exclude aden-lsp --quiet 2>&1 | tail -5
	$(INSTALL_SCRIPT)
	@echo "Installed (debug)"

# Release build + install. Use before sharing or benchmarking.
release:
	INSTALL_WAS_DONE=1 $(INSTALL_SCRIPT)
	@echo "Installed (release)"

# Run tests only (no install).
test:
	cargo test --workspace --exclude aden-lsp

# Build only (no install, no test).
build:
	cargo build --workspace --exclude aden-lsp

# Full CI gates (check + heal + test).
ci:
	cargo fmt --all -- --check
	cargo clippy --workspace -- -D warnings
	cargo build --workspace --exclude aden-lsp
	cargo test --workspace --exclude aden-lsp
	aden check .

# Build the standalone LSP server (excluded from the default workspace build).
lsp:
	cargo build -p aden-lsp

# Install git hooks (pre-commit for secret scan + aden check + test).
install-hooks:
	mkdir -p .git/hooks
	cp tools/git-hooks/pre-commit .git/hooks/pre-commit
	chmod +x .git/hooks/pre-commit
	@echo "Pre-commit hook installed from tools/git-hooks/pre-commit"
	@echo "Run 'git config core.hooksPath git-hooks' if you also want the pre-push hook active (currently in git-hooks/)."

# Quick one-command gate (local convenience).
ready:
	aden ready .

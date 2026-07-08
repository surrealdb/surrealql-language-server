# Makefile for the SurrealQL Language Server.
#
# Convenience wrapper around the native cargo builds and the browser
# WebAssembly packaging pipeline (scripts/build-wasm.sh).
#
# The tree-sitter grammar must be available as a sibling checkout at
# ../surrealql-tree-sitter (or via TREE_SITTER_SURREALQL_DIR). Run
# `make grammar` once to fetch it before building.

CARGO       ?= cargo
WASM_TARGET := wasm32-unknown-unknown
OUT_NAME    := surrealql_language_server

.DEFAULT_GOAL := help

.PHONY: help
help: ## Show this help
	@awk 'BEGIN {FS = ":.*## "; printf "Usage: make <target>\n\nTargets:\n"} /^[a-zA-Z0-9_-]+:.*## / {printf "  \033[36m%-14s\033[0m %s\n", $$1, $$2}' $(MAKEFILE_LIST)

.PHONY: grammar
grammar: ## Clone/update the sibling tree-sitter grammar
	bash scripts/setup-grammar.sh

.PHONY: build
build: ## Build the native binary + library (debug)
	$(CARGO) build

.PHONY: debug
debug: build ## Alias for `build` (debug profile)

.PHONY: release
release: ## Build the native binary + library (release)
	$(CARGO) build --release

.PHONY: wasm
wasm: ## Build the browser WASM module and refresh pkg/
	bash scripts/build-wasm.sh

.PHONY: wasm-setup
wasm-setup: ## Install the WASM toolchain prerequisites (target, CLIs, clang)
	rustup target add $(WASM_TARGET)
	$(CARGO) install -f wasm-bindgen-cli --version 0.2.108
	$(CARGO) install wasm-opt
	@if [ "$$(uname)" = "Darwin" ] && ! ls /opt/homebrew/opt/llvm/bin/clang /usr/local/opt/llvm/bin/clang /usr/local/llvm/bin/clang >/dev/null 2>&1; then \
		echo "Installing Homebrew LLVM (provides a wasm-capable clang)"; \
		brew install llvm; \
	fi

.PHONY: test
test: ## Run the test suite
	$(CARGO) test

.PHONY: fmt
fmt: ## Format the code
	$(CARGO) fmt

.PHONY: fmt-check
fmt-check: ## Check formatting without modifying files (CI parity)
	$(CARGO) fmt --check

.PHONY: clean
clean: ## Remove cargo artifacts and generated pkg/ output
	$(CARGO) clean
	rm -f pkg/$(OUT_NAME).js pkg/$(OUT_NAME).d.ts \
		pkg/$(OUT_NAME)_bg.wasm pkg/$(OUT_NAME)_bg.wasm.d.ts pkg/LICENSE

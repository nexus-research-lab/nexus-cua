.DEFAULT_GOAL := help

CARGO ?= cargo
DEV_ROOT ?= .cache/dev

.PHONY: help install dev run doctor build release check fmt format lint test schema clean

help: ## Show available commands
	@awk 'BEGIN {FS = ":.*##"; printf "\nUsage:\n  make \033[36m<target>\033[0m\n\nTargets:\n"} /^[a-zA-Z_-]+:.*?##/ { printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2 }' $(MAKEFILE_LIST)

install: ## Fetch pinned Rust dependencies
	$(CARGO) fetch --locked

dev: ## Start an isolated debug service (Ctrl-C to stop)
	@echo "Starting Nexus CUA development service"
	@echo "State: $(DEV_ROOT)"
	$(CARGO) run --package nexus-cua -- --log-level debug serve --dev-root "$(DEV_ROOT)"

run: ## Run the CLI; pass ARGS='doctor'
	$(CARGO) run --package nexus-cua -- $(ARGS)

doctor: ## Inspect native driver capabilities and permissions
	$(CARGO) run --package nexus-cua -- doctor

build: ## Build the complete workspace
	$(CARGO) build --workspace

release: ## Build an optimized service binary
	$(CARGO) build --workspace --release

check: fmt lint test ## Run the local quality gate

fmt: ## Check Rust formatting without changing files
	$(CARGO) fmt --all -- --check

format: ## Format Rust source files
	$(CARGO) fmt --all

lint: ## Run Clippy across all targets with warnings denied
	$(CARGO) clippy --workspace --all-targets -- -D warnings

test: ## Run workspace unit, contract, and integration tests
	$(CARGO) test --workspace

schema: ## Print the versioned request and response JSON schemas
	$(CARGO) run --quiet --package nexus-cua -- schema

clean: ## Remove Rust build output only
	$(CARGO) clean

.DEFAULT_GOAL := help

CARGO ?= cargo
PACKAGE_FLAGS ?=
DEV_ROOT ?= .cache/dev
SCHEMA_DIR ?= schemas/nexus.cua.v1
PACKAGE_ROOT := $(abspath target/package)
PROTOCOL_PACKAGE := $(PACKAGE_ROOT)/nexus-cua-protocol-0.1.0
RUNTIME_PACKAGE := $(PACKAGE_ROOT)/nexus-cua-runtime-0.1.0
PLATFORM_PACKAGE := $(PACKAGE_ROOT)/nexus-cua-platform-0.1.0
TRANSPORT_PACKAGE := $(PACKAGE_ROOT)/nexus-cua-transport-0.1.0
CLI_PACKAGE := $(PACKAGE_ROOT)/nexus-cua-0.1.0

.PHONY: help install dev run doctor build release package package-verify check fmt format lint test smoke schema schema-write schema-check docs clean

help: ## Show available commands
	@awk 'BEGIN {FS = ":.*##"; printf "\nUsage:\n  make \033[36m<target>\033[0m\n\nTargets:\n"} /^[a-zA-Z_-]+:.*?##/ { printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2 }' $(MAKEFILE_LIST)

install: ## Fetch pinned Rust dependencies
	$(CARGO) fetch --locked

dev: ## Start an isolated debug service (Ctrl-C to stop)
	@echo "Starting Nexus Computer Use development service"
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

package: ## Build publishable crate source archives
	$(CARGO) package --workspace --no-verify --locked $(PACKAGE_FLAGS)

package-verify: package ## Compile each archive against this workspace's packaged dependencies
	rm -rf "$(PROTOCOL_PACKAGE)" "$(RUNTIME_PACKAGE)" "$(PLATFORM_PACKAGE)" \
		"$(TRANSPORT_PACKAGE)" "$(CLI_PACKAGE)"
	tar -xzf "$(PACKAGE_ROOT)/nexus-cua-protocol-0.1.0.crate" -C "$(PACKAGE_ROOT)"
	tar -xzf "$(PACKAGE_ROOT)/nexus-cua-runtime-0.1.0.crate" -C "$(PACKAGE_ROOT)"
	tar -xzf "$(PACKAGE_ROOT)/nexus-cua-platform-0.1.0.crate" -C "$(PACKAGE_ROOT)"
	tar -xzf "$(PACKAGE_ROOT)/nexus-cua-transport-0.1.0.crate" -C "$(PACKAGE_ROOT)"
	tar -xzf "$(PACKAGE_ROOT)/nexus-cua-0.1.0.crate" -C "$(PACKAGE_ROOT)"
	cp Cargo.lock "$(PROTOCOL_PACKAGE)/Cargo.lock"
	$(CARGO) check --manifest-path "$(PROTOCOL_PACKAGE)/Cargo.toml" --offline
	$(CARGO) check --manifest-path "$(PROTOCOL_PACKAGE)/Cargo.toml" --locked --offline
	cp Cargo.lock "$(RUNTIME_PACKAGE)/Cargo.lock"
	$(CARGO) check --manifest-path "$(RUNTIME_PACKAGE)/Cargo.toml" --offline \
		--config 'patch.crates-io.nexus-cua-protocol.path="$(PROTOCOL_PACKAGE)"'
	$(CARGO) check --manifest-path "$(RUNTIME_PACKAGE)/Cargo.toml" --locked --offline \
		--config 'patch.crates-io.nexus-cua-protocol.path="$(PROTOCOL_PACKAGE)"'
	cp Cargo.lock "$(PLATFORM_PACKAGE)/Cargo.lock"
	$(CARGO) check --manifest-path "$(PLATFORM_PACKAGE)/Cargo.toml" --offline \
		--config 'patch.crates-io.nexus-cua-protocol.path="$(PROTOCOL_PACKAGE)"' \
		--config 'patch.crates-io.nexus-cua-runtime.path="$(RUNTIME_PACKAGE)"'
	$(CARGO) check --manifest-path "$(PLATFORM_PACKAGE)/Cargo.toml" --locked --offline \
		--config 'patch.crates-io.nexus-cua-protocol.path="$(PROTOCOL_PACKAGE)"' \
		--config 'patch.crates-io.nexus-cua-runtime.path="$(RUNTIME_PACKAGE)"'
	cp Cargo.lock "$(TRANSPORT_PACKAGE)/Cargo.lock"
	$(CARGO) check --manifest-path "$(TRANSPORT_PACKAGE)/Cargo.toml" --offline \
		--config 'patch.crates-io.nexus-cua-protocol.path="$(PROTOCOL_PACKAGE)"' \
		--config 'patch.crates-io.nexus-cua-runtime.path="$(RUNTIME_PACKAGE)"'
	$(CARGO) check --manifest-path "$(TRANSPORT_PACKAGE)/Cargo.toml" --locked --offline \
		--config 'patch.crates-io.nexus-cua-protocol.path="$(PROTOCOL_PACKAGE)"' \
		--config 'patch.crates-io.nexus-cua-runtime.path="$(RUNTIME_PACKAGE)"'
	cp Cargo.lock "$(CLI_PACKAGE)/Cargo.lock"
	$(CARGO) check --manifest-path "$(CLI_PACKAGE)/Cargo.toml" --offline \
		--config 'patch.crates-io.nexus-cua-platform.path="$(PLATFORM_PACKAGE)"' \
		--config 'patch.crates-io.nexus-cua-protocol.path="$(PROTOCOL_PACKAGE)"' \
		--config 'patch.crates-io.nexus-cua-runtime.path="$(RUNTIME_PACKAGE)"' \
		--config 'patch.crates-io.nexus-cua-transport.path="$(TRANSPORT_PACKAGE)"'
	$(CARGO) check --manifest-path "$(CLI_PACKAGE)/Cargo.toml" --locked --offline \
		--config 'patch.crates-io.nexus-cua-platform.path="$(PLATFORM_PACKAGE)"' \
		--config 'patch.crates-io.nexus-cua-protocol.path="$(PROTOCOL_PACKAGE)"' \
		--config 'patch.crates-io.nexus-cua-runtime.path="$(RUNTIME_PACKAGE)"' \
		--config 'patch.crates-io.nexus-cua-transport.path="$(TRANSPORT_PACKAGE)"'

check: fmt lint test schema-check docs ## Run the local quality gate

fmt: ## Check Rust formatting without changing files
	$(CARGO) fmt --all -- --check

format: ## Format Rust source files
	$(CARGO) fmt --all

lint: ## Run Clippy across all targets with warnings denied
	$(CARGO) clippy --workspace --all-targets -- -D warnings

test: ## Run workspace unit, contract, and integration tests
	$(CARGO) test --workspace

smoke: ## Exercise the native local IPC service end to end
	$(CARGO) test --package nexus-cua-transport --test local_ipc

schema: ## Print the versioned request and response JSON schemas
	$(CARGO) run --quiet --package nexus-cua -- schema

schema-write: ## Regenerate committed protocol schemas
	$(CARGO) run --quiet --package nexus-cua -- schema --output-dir "$(SCHEMA_DIR)"

schema-check: ## Fail when committed protocol schemas drift
	@schema_tmp=$$(mktemp -d); \
	trap 'rm -rf "$$schema_tmp"' EXIT; \
	$(CARGO) run --quiet --package nexus-cua -- schema --output-dir "$$schema_tmp"; \
	diff -ru "$(SCHEMA_DIR)" "$$schema_tmp"

docs: ## Validate local Markdown paths and anchors
	ruby scripts/check_markdown_links.rb

clean: ## Remove Rust build output only
	$(CARGO) clean

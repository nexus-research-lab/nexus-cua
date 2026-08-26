.DEFAULT_GOAL := help

CARGO ?= cargo
GO ?= go
GOFMT ?= gofmt
PYTHON ?= python3
PACKAGE_FLAGS ?=
PYTHON_PACKAGE_FLAGS ?=
DEV_ROOT ?= .cache/dev
SCHEMA_DIR ?= schemas/nexus.cua.v1
PACKAGE_ROOT := $(abspath target/package)
PROTOCOL_PACKAGE := $(PACKAGE_ROOT)/nexus-cua-protocol-0.1.0
RUNTIME_PACKAGE := $(PACKAGE_ROOT)/nexus-cua-runtime-0.1.0
PLATFORM_PACKAGE := $(PACKAGE_ROOT)/nexus-cua-platform-0.1.0
TRANSPORT_PACKAGE := $(PACKAGE_ROOT)/nexus-cua-transport-0.1.0
CLI_PACKAGE := $(PACKAGE_ROOT)/nexus-cua-0.1.0
NATIVE_HARNESS_MANIFEST := tools/native-harness/Cargo.toml
NATIVE_ENDPOINT ?=
NATIVE_TOKEN_FILE ?=
NATIVE_SERVICE_PID ?=
NATIVE_RUNNER_MANIFEST ?=
NATIVE_STATE_FILE ?= target/native-evidence/restart.json
NATIVE_SOAK_PROFILE ?= diagnostic
NATIVE_SOAK_FLAGS ?=
NATIVE_BENCHMARK_FLAGS ?=
NATIVE_FAULT_MODE ?= preflight
NATIVE_FAULT_FLAGS ?=
NATIVE_PERMISSION_FLAGS ?=
NATIVE_EVIDENCE_DIR ?= target/native-evidence
NATIVE_SOURCE_REVISION ?=
NATIVE_RUNTIME_SHA256 ?=
NATIVE_EVIDENCE_FLAGS ?=
FIXTURE_CONFIGURATION ?= debug

.PHONY: help install dev run doctor build release package package-verify check fmt format lint test smoke schema schema-write schema-check docs sdk-check go-check python-check python-package-verify native-harness-check native-fixture-macos native-validate native-benchmark native-soak native-fault native-permission native-evidence native-restart-prepare native-restart-verify clean

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

check: fmt lint test schema-check docs native-harness-check sdk-check ## Run the local quality gate

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

sdk-check: go-check python-check ## Validate official Go and Python clients

go-check: ## Format, vet, and test the official Go client
	@test -z "$$($(GOFMT) -l sdk/go)" || ($(GOFMT) -l sdk/go >&2; exit 1)
	cd sdk/go && $(GO) mod verify
	cd sdk/go && $(GO) vet ./...
	cd sdk/go && $(GO) test ./...

python-check: ## Compile and test the official Python client
	PYTHONPATH=sdk/python/src $(PYTHON) -m compileall -q \
		sdk/python/src sdk/python/tests sdk/python/examples
	PYTHONPATH=sdk/python/src $(PYTHON) -m unittest discover -s sdk/python/tests -v

python-package-verify: ## Build the Python wheel from the declared package metadata
	rm -rf target/python-package
	$(PYTHON) -m pip wheel --no-deps $(PYTHON_PACKAGE_FLAGS) \
		--wheel-dir target/python-package sdk/python

native-harness-check: ## Format, lint, and test the standalone native validation harness
	$(CARGO) fmt --manifest-path "$(NATIVE_HARNESS_MANIFEST)" -- --check
	$(CARGO) clippy --manifest-path "$(NATIVE_HARNESS_MANIFEST)" --all-targets --locked -- -D warnings
	$(CARGO) test --manifest-path "$(NATIVE_HARNESS_MANIFEST)" --locked

native-fixture-macos: ## Build the deterministic AppKit fixture application
	fixtures/native/macos/build.sh "$(FIXTURE_CONFIGURATION)"

native-validate: ## Run fixture validation; set NATIVE_ENDPOINT and NATIVE_TOKEN_FILE
	@test -n "$(NATIVE_ENDPOINT)" || (echo "NATIVE_ENDPOINT is required" >&2; exit 2)
	@test -n "$(NATIVE_TOKEN_FILE)" || (echo "NATIVE_TOKEN_FILE is required" >&2; exit 2)
	$(CARGO) run --quiet --locked --manifest-path "$(NATIVE_HARNESS_MANIFEST)" -- \
		--endpoint "$(NATIVE_ENDPOINT)" --token-file "$(NATIVE_TOKEN_FILE)" validate

native-benchmark: ## Benchmark the native fixture against a maintained runner manifest
	@test -n "$(NATIVE_ENDPOINT)" || (echo "NATIVE_ENDPOINT is required" >&2; exit 2)
	@test -n "$(NATIVE_TOKEN_FILE)" || (echo "NATIVE_TOKEN_FILE is required" >&2; exit 2)
	@test -n "$(NATIVE_RUNNER_MANIFEST)" || (echo "NATIVE_RUNNER_MANIFEST is required" >&2; exit 2)
	$(CARGO) run --quiet --release --locked --manifest-path "$(NATIVE_HARNESS_MANIFEST)" -- \
		--endpoint "$(NATIVE_ENDPOINT)" --token-file "$(NATIVE_TOKEN_FILE)" benchmark \
		--runner-manifest "$(NATIVE_RUNNER_MANIFEST)" $(NATIVE_BENCHMARK_FLAGS)

native-soak: ## Run a diagnostic, idle, engineering, or release native soak
	@test -n "$(NATIVE_ENDPOINT)" || (echo "NATIVE_ENDPOINT is required" >&2; exit 2)
	@test -n "$(NATIVE_TOKEN_FILE)" || (echo "NATIVE_TOKEN_FILE is required" >&2; exit 2)
	@test -n "$(NATIVE_SERVICE_PID)" || (echo "NATIVE_SERVICE_PID is required" >&2; exit 2)
	@test -n "$(NATIVE_RUNNER_MANIFEST)" || (echo "NATIVE_RUNNER_MANIFEST is required" >&2; exit 2)
	$(CARGO) run --quiet --release --locked --manifest-path "$(NATIVE_HARNESS_MANIFEST)" -- \
		--endpoint "$(NATIVE_ENDPOINT)" --token-file "$(NATIVE_TOKEN_FILE)" soak \
		--profile "$(NATIVE_SOAK_PROFILE)" --service-pid "$(NATIVE_SERVICE_PID)" \
		--runner-manifest "$(NATIVE_RUNNER_MANIFEST)" $(NATIVE_SOAK_FLAGS)

native-fault: ## Run a controlled preflight or post-dispatch provider fault probe
	@test -n "$(NATIVE_ENDPOINT)" || (echo "NATIVE_ENDPOINT is required" >&2; exit 2)
	@test -n "$(NATIVE_TOKEN_FILE)" || (echo "NATIVE_TOKEN_FILE is required" >&2; exit 2)
	$(CARGO) run --quiet --release --locked --manifest-path "$(NATIVE_HARNESS_MANIFEST)" -- \
		--endpoint "$(NATIVE_ENDPOINT)" --token-file "$(NATIVE_TOKEN_FILE)" \
		fault "$(NATIVE_FAULT_MODE)" $(NATIVE_FAULT_FLAGS)

native-permission: ## Run a permission/protected-target probe using NATIVE_PERMISSION_FLAGS
	@test -n "$(NATIVE_ENDPOINT)" || (echo "NATIVE_ENDPOINT is required" >&2; exit 2)
	@test -n "$(NATIVE_TOKEN_FILE)" || (echo "NATIVE_TOKEN_FILE is required" >&2; exit 2)
	@test -n "$(NATIVE_PERMISSION_FLAGS)" || (echo "NATIVE_PERMISSION_FLAGS is required" >&2; exit 2)
	$(CARGO) run --quiet --release --locked --manifest-path "$(NATIVE_HARNESS_MANIFEST)" -- \
		--endpoint "$(NATIVE_ENDPOINT)" --token-file "$(NATIVE_TOKEN_FILE)" \
		permission $(NATIVE_PERMISSION_FLAGS)

native-evidence: ## Aggregate raw native reports into one release-gate summary
	@test -n "$(NATIVE_RUNNER_MANIFEST)" || (echo "NATIVE_RUNNER_MANIFEST is required" >&2; exit 2)
	@test -n "$(NATIVE_SOURCE_REVISION)" || (echo "NATIVE_SOURCE_REVISION is required" >&2; exit 2)
	@test -n "$(NATIVE_RUNTIME_SHA256)" || (echo "NATIVE_RUNTIME_SHA256 is required" >&2; exit 2)
	$(CARGO) run --quiet --release --locked --manifest-path "$(NATIVE_HARNESS_MANIFEST)" -- \
		evidence --evidence-dir "$(NATIVE_EVIDENCE_DIR)" \
		--runner-manifest "$(NATIVE_RUNNER_MANIFEST)" \
		--source-revision "$(NATIVE_SOURCE_REVISION)" \
		--runtime-sha256 "$(NATIVE_RUNTIME_SHA256)" $(NATIVE_EVIDENCE_FLAGS)

native-restart-prepare: ## Prepare a live session/artifact before an orchestrated sidecar restart
	@test -n "$(NATIVE_ENDPOINT)" || (echo "NATIVE_ENDPOINT is required" >&2; exit 2)
	@test -n "$(NATIVE_TOKEN_FILE)" || (echo "NATIVE_TOKEN_FILE is required" >&2; exit 2)
	$(CARGO) run --quiet --release --locked --manifest-path "$(NATIVE_HARNESS_MANIFEST)" -- \
		--endpoint "$(NATIVE_ENDPOINT)" --token-file "$(NATIVE_TOKEN_FILE)" \
		restart-prepare --state-file "$(NATIVE_STATE_FILE)"

native-restart-verify: ## Verify old session/artifact state after sidecar restart
	@test -n "$(NATIVE_ENDPOINT)" || (echo "NATIVE_ENDPOINT is required" >&2; exit 2)
	@test -n "$(NATIVE_TOKEN_FILE)" || (echo "NATIVE_TOKEN_FILE is required" >&2; exit 2)
	$(CARGO) run --quiet --release --locked --manifest-path "$(NATIVE_HARNESS_MANIFEST)" -- \
		--endpoint "$(NATIVE_ENDPOINT)" --token-file "$(NATIVE_TOKEN_FILE)" \
		restart-verify --state-file "$(NATIVE_STATE_FILE)"

clean: ## Remove Rust build output only
	$(CARGO) clean

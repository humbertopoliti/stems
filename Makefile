# stems — single entry point for local checks and CI.
# `make check` is the full local suite (process tests, e2e, example services);
# `make ci` is the regression subset CI runs: no processes, no examples, no Docker.

SHELL := /bin/sh
CARGO ?= cargo
PYTHON ?= python3

# Optional e2e filters, forwarded to the harness as environment variables.
FEATURE ?=
TAGS ?=
E2E_ENV = STEMS_E2E_FEATURE="$(FEATURE)" STEMS_E2E_TAGS="$(TAGS)"

.PHONY: check check-docker ci fmt fmt-check clippy test test-unit test-python lint-yaml \
        build e2e e2e-docker e2e-selftest trace docs \
        test-release smoke size dist-plan

check: fmt-check clippy test test-python test-release lint-yaml e2e e2e-selftest trace

check-docker: check e2e-docker

ci: fmt-check clippy test-unit test-release lint-yaml trace

fmt:
	$(CARGO) fmt --all

fmt-check:
	$(CARGO) fmt --all -- --check

clippy:
	$(CARGO) clippy --workspace --all-targets --all-features -- -D warnings

# Everything, including the integration tests that start real processes.
test:
	$(CARGO) test --workspace --all-features

# Unit and in-process tests only (CI). cargo-nextest enforces a per-test
# timeout (.config/nextest.toml); it doesn't run doctests, so those run
# separately. NEXTEST_PROFILE=ci for verbose per-test output.
NEXTEST_PROFILE ?= default
test-unit:
	$(CARGO) nextest run --workspace --profile $(NEXTEST_PROFILE)
	$(CARGO) test --workspace --doc

# Python unit tests for the example service repos (deliverable 02+).
test-python:
	@found=0; \
	for d in examples/repos/*/tests; do \
		[ -d "$$d" ] || continue; \
		ls "$$d"/test*.py >/dev/null 2>&1 || continue; \
		found=1; \
		echo "python unittest: $$d"; \
		(cd "$$(dirname "$$d")" && $(PYTHON) -m unittest discover -s tests) || exit 1; \
	done; \
	[ "$$found" -eq 1 ] || echo "test-python: no examples/repos/*/tests yet, skipping"

# YAML lint (script lands in deliverable 03).
lint-yaml:
	@if [ -f scripts/lint_yaml.py ]; then \
		$(PYTHON) scripts/lint_yaml.py; \
	else \
		echo "lint-yaml: scripts/lint_yaml.py not present yet, skipping"; \
	fi

build:
	$(CARGO) build --workspace --release

# E2E tiers (deliverable 04): cucumber-rs harness in tests/stems-e2e driving
# the real `stems` binary. Build the binary first; the harness locates it at
# target/<profile>/stems. Scenarios listed in tests/features/PENDING.txt are
# run and may fail ("PENDING (expected)") but must not pass.
#   make e2e FEATURE=tests/features/foo.feature   one file or directory
#   make e2e TAGS='@FR-LC-5 and not @slow'        a tag expression
# Runs go through scripts/e2e_slot.sh: up to STEMS_E2E_SLOTS (default 4)
# concurrent runs per machine, each with its own port range and temp-dir
# prefix; STEMS_E2E_CONCURRENCY (default 8) scenarios in parallel per run.
E2E_RUN = scripts/e2e_slot.sh $(CARGO) test -p stems-e2e --test e2e --
# The Docker tier holds every slot: compose projects are machine-global.
E2E_RUN_EXCLUSIVE = scripts/e2e_slot.sh --exclusive $(CARGO) test -p stems-e2e --test e2e --

# Fast tier: everything except @docker and @harness-selftest.
e2e:
	$(CARGO) build -p stems-cli
	$(E2E_ENV) $(E2E_RUN)

# Docker tier: also @docker scenarios (run serially; needs a Docker daemon
# with compose v2). Image pulls/builds are slow: 300 s per scenario unless
# STEMS_E2E_SCENARIO_TIMEOUT says otherwise (see docs/docker.md).
e2e-docker:
	$(CARGO) build -p stems-cli
	$(E2E_ENV) STEMS_E2E_DOCKER=1 STEMS_E2E_SCENARIO_TIMEOUT=$${STEMS_E2E_SCENARIO_TIMEOUT:-300} $(E2E_RUN_EXCLUSIVE)

# Harness self-test: runs only @harness-selftest and passes iff the After
# hook reported LEAK: (it then kills the stray process itself).
e2e-selftest:
	$(CARGO) build -p stems-cli
	$(E2E_ENV) STEMS_E2E_SELFTEST=1 $(E2E_RUN)

# Requirement traceability plus its unit tests. The requirements and plan
# files are maintainer-local (not in the repo); without them only the unit
# tests run.
trace:
	$(PYTHON) -m unittest discover -s scripts/tests
	@if [ -f REQUIREMENTS.md ] && [ -d plan ]; then \
		$(PYTHON) scripts/trace.py --quiet; \
	else \
		echo "trace: REQUIREMENTS.md/plan/ not present, skipping the traceability check"; \
	fi

# API docs, plus docs/cli.md generated from the clap tree (`stems __docs`);
# crates/stems-cli/tests/docs.rs fails when docs/cli.md drifts.
docs:
	$(CARGO) doc --workspace --no-deps
	$(CARGO) run -q -p stems-cli -- __docs > docs/cli.md

# --- release (deliverable 32; see release/RELEASING.md) ----------------------

# Formula template golden and render script tests.
test-release:
	$(PYTHON) -m unittest discover -s release/tests

# Smoke-test an installed or built binary end to end (Docker-free):
#   make smoke                         builds target/release/stems first
#   make smoke BIN=/opt/homebrew/bin/stems SMOKE_BREW=1
BIN ?=
smoke:
	@if [ -z "$(BIN)" ]; then $(CARGO) build --release -p stems-cli; fi
	release/smoke.sh $(if $(BIN),$(BIN),target/release/stems)

# Binary size budget: the release binary must stay under 25 MB
# (strip, fat LTO and codegen-units = 1 in [profile.release]).
SIZE_BUDGET_BYTES ?= 26214400
size:
	$(CARGO) build --release -p stems-cli
	@bytes=$$(wc -c < target/release/stems | tr -d ' '); \
	echo "target/release/stems: $$bytes bytes ($$((bytes / 1048576)) MiB; budget $$(($(SIZE_BUDGET_BYTES) / 1048576)) MiB)"; \
	[ "$$bytes" -le $(SIZE_BUDGET_BYTES) ] || { echo "size: over budget" >&2; exit 1; }

# What a tag would build and publish (needs `dist`: cargo install cargo-dist).
dist-plan:
	dist plan

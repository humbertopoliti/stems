# stems — single entry point for local checks and CI.
# `make check` is the definition of green for every deliverable.

SHELL := /bin/sh
CARGO ?= cargo
PYTHON ?= python3

# Optional e2e filters, forwarded to the harness as environment variables.
FEATURE ?=
TAGS ?=
E2E_ENV = STEMS_E2E_FEATURE="$(FEATURE)" STEMS_E2E_TAGS="$(TAGS)"

.PHONY: check check-docker fmt fmt-check clippy test test-python lint-yaml \
        build e2e e2e-docker e2e-selftest trace docs

check: fmt-check clippy test test-python lint-yaml e2e e2e-selftest trace

check-docker: check e2e-docker

fmt:
	$(CARGO) fmt --all

fmt-check:
	$(CARGO) fmt --all -- --check

clippy:
	$(CARGO) clippy --workspace --all-targets -- -D warnings

test:
	$(CARGO) test --workspace

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
E2E_RUN = $(CARGO) test -p stems-e2e --test e2e --

# Fast tier: everything except @docker and @harness-selftest.
e2e:
	$(CARGO) build -p stems-cli
	$(E2E_ENV) $(E2E_RUN)

# Docker tier: also @docker scenarios (run serially; needs a Docker daemon).
e2e-docker:
	$(CARGO) build -p stems-cli
	$(E2E_ENV) STEMS_E2E_DOCKER=1 $(E2E_RUN)

# Harness self-test: runs only @harness-selftest and passes iff the After
# hook reported LEAK: (it then kills the stray process itself).
e2e-selftest:
	$(CARGO) build -p stems-cli
	$(E2E_ENV) STEMS_E2E_SELFTEST=1 $(E2E_RUN)

# Requirement traceability (REQUIREMENTS.md 7.4) plus its unit tests.
trace:
	$(PYTHON) -m unittest discover -s scripts/tests
	$(PYTHON) scripts/trace.py --quiet

# API docs, plus docs/cli.md generated from the clap tree (`stems __docs`);
# crates/stems-cli/tests/docs.rs fails when docs/cli.md drifts.
docs:
	$(CARGO) doc --workspace --no-deps
	$(CARGO) run -q -p stems-cli -- __docs > docs/cli.md

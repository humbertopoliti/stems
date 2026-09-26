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

check: fmt-check clippy test test-python lint-yaml e2e trace

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

# Fast e2e tier (cucumber harness lands in deliverable 04; placeholder for now).
e2e:
	$(E2E_ENV) $(CARGO) test -p stems-e2e

# Docker e2e tier (@docker scenarios; needs a Docker daemon).
e2e-docker:
	$(E2E_ENV) STEMS_E2E_DOCKER=1 $(CARGO) test -p stems-e2e

# Harness self-tests (deliverable 04).
e2e-selftest:
	$(E2E_ENV) STEMS_E2E_SELFTEST=1 $(CARGO) test -p stems-e2e

trace:
	$(PYTHON) scripts/trace.py

docs:
	$(CARGO) doc --workspace --no-deps

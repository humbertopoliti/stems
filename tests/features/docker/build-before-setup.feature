@docker @FR-ST-3
Feature: a docker stem built before setup
  `build: { before_setup: true }` on a docker stem builds its image (tag
  `stems/<ws>/<stem>:<run_id>`) at the top of the start sequence, before
  `setup`, which gets the tag as `STEMS_IMAGE`; the container start that
  follows runs that image without building it again. Needs Docker:
  `make e2e-docker`.

  Scenario: the image is built once, before setup, and setup sees STEMS_IMAGE
    Given the fixture workspace "docker-build-before-setup"
    When I run "stems up --detach --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["api"]
    And the events stream contains {"kind": "docker.build", "stem": "api"} before {"kind": "stem.state", "stem": "api", "to": "setup"}
    And there are exactly 1 events matching {"kind": "docker.build", "stem": "api", "data": {"line": "Step 1/9 : FROM python:3-slim"}}
    When I run "stems logs api --script setup --json"
    Then the JSON at "$..text" contains "STEMS_IMAGE=stems/docker-build-before-setup/api:"
    When I run the shell command "docker inspect -f '{{.Config.Image}}' docker-build-before-setup-api"
    Then the exit code is 0
    And stdout contains "stems/docker-build-before-setup/api:"
    When I run the shell command "curl -fsS http://127.0.0.1:${port:18536}/healthz"
    Then the exit code is 0
    When I run "stems down --json"
    Then the command succeeds
    And no container with label stems.workspace=docker-build-before-setup exists
    When I run the shell command "docker image ls -q stems/docker-build-before-setup/api | xargs docker image rm -f"

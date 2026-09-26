@docker @FR-ST-3
Feature: a docker stem built from a Dockerfile
  `build: { context }` builds the context through the Docker API (tag
  `stems/<ws>/<stem>:<run_id>`, `docker.build` events per output line) and
  runs the image. Needs Docker: `make e2e-docker`.

  Scenario: shop-api built from its Dockerfile serves /healthz
    Given the fixture workspace "docker-build"
    When I run "stems up --detach --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["api"]
    And within 5s the events stream contains {"kind": "docker.build", "stem": "api"}
    And the container "docker-build-api" is running with label "stems.stem=api"
    When I run the shell command "docker inspect -f '{{.Config.Image}}' docker-build-api"
    Then the exit code is 0
    And stdout contains "stems/docker-build/api:"
    When I run the shell command "curl -fsS http://127.0.0.1:${port:18533}/healthz"
    Then the exit code is 0
    When I run "stems down --json"
    Then the command succeeds
    And no container with label stems.workspace=docker-build exists
    When I run the shell command "docker image ls -q stems/docker-build/api | xargs docker image rm -f"

@docker @FR-ST-3 @FR-ST-8
Feature: switch a stem from a local process to a docker container and back
  The `docker` variant of `api` builds examples/repos/shop-api's Dockerfile
  from the stem's own codebase (`build: { context: "${codebase}" }`) and
  runs it as a container on the same host port; `stems switch api local`
  brings the process back and removes the container. Needs Docker:
  `make e2e-docker`. Fixture: tests/fixtures/workspaces/variants-demo.

  Scenario: api becomes a container, keeps serving, and comes back
    Given the fixture workspace "variants-demo"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name=='api'].type" equals "process"
    When I save the JSON at "$.data.stems[?@.name=='web'].pid" as "web_pid"
    When I run "stems switch api docker --json"
    Then the command succeeds
    And the JSON at "$.data.type" equals "docker"
    And the JSON at "$.data.applied.applied[?@.stem=='api'].result" equals "restarted"
    And the container "variants-demo-api" is running with label "stems.stem=api"
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name=='api'].type" equals "docker"
    And the JSON at "$.data.stems[?@.name=='api'].variant" equals "docker"
    And the JSON at "$.data.stems[?@.name=='api'].state" equals "healthy"
    And the JSON at "$.data.stems[?@.name=='api'].pid" equals null
    And the JSON at "$.data.stems[?@.name=='web'].pid" equals ${var:web_pid}
    When I run the shell command "curl -fsS http://127.0.0.1:${port:18651}/healthz"
    Then the exit code is 0
    When I run "stems switch api local --json"
    Then the command succeeds
    And the JSON at "$.data.type" equals "process"
    And no container with label stems.workspace=variants-demo exists
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name=='api'].type" equals "process"
    And the JSON at "$.data.stems[?@.name=='api'].state" equals "healthy"
    When I run the shell command "curl -fsS http://127.0.0.1:${port:18651}/healthz"
    Then the exit code is 0
    When I run "stems down --all --json"
    Then the command succeeds
    And no container with label stems.workspace=variants-demo exists
    When I run the shell command "docker image ls -q stems/variants-demo/api | xargs docker image rm -f"

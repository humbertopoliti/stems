@docker @FR-CL-3
Feature: doctor and Docker (needs Docker: `make e2e-docker`)

  @error
  Scenario: an unreachable Docker fails docker.reachable
    Given the fixture workspace "docker-mixed"
    When I run "stems doctor --json" with env DOCKER_HOST=unix:///nonexistent
    Then the exit code is 1
    And the JSON at "$.data.checks[?@.id == 'docker.reachable'].status" equals "fail"
    And the JSON error has code "DOCKER_UNAVAILABLE"

  Scenario: a reachable Docker reports its version
    Given the fixture workspace "docker-mixed"
    When I run "stems doctor --json"
    Then the JSON at "$.data.checks[?@.id == 'docker.reachable'].status" equals "ok"
    And the JSON at "$.data.checks[?@.id == 'docker.version'].status" equals "ok"
    And the JSON at "$.data.checks[?@.id == 'docker.version'].details.version" exists

  @FR-CR-4
  Scenario: a stopped stems-labelled container is pruned by --fix
    Given the fixture workspace "docker-mixed"
    When I run the shell command "docker create --label stems.workspace=docker-mixed --label stems.stem=db --name docker-mixed-db-orphan alpine:3 true"
    Then the command succeeds
    When I run "stems doctor --json"
    Then the command succeeds
    And the JSON at "$.data.checks[?@.id == 'orphans'].status" equals "warn"
    And the JSON at "$.data.checks[?@.id == 'orphans'].fixable" equals true
    And the JSON at "$.data.checks[?@.id == 'orphans'].details.orphans[0].kind" equals "container"
    When I run "stems doctor --fix --yes --json"
    Then the command succeeds
    And the JSON at "$.data.fixed[?@.action == 'remove_container'].ok" equals true
    And the JSON at "$.data.checks[?@.id == 'orphans'].status" equals "ok"
    And no container with label stems.workspace=docker-mixed exists

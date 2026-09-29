@FR-ST-3
Feature: stems pull (the half that runs without Docker)
  Selection and errors of `stems pull`; the pulls themselves are in
  tests/features/docker/pull.feature (`make e2e-docker`).

  @error
  Scenario: unknown stems are a usage-level error
    Given the fixture workspace "docker-mixed"
    When I run "stems pull nope --json"
    Then the exit code is 2
    And the JSON error has code "UNKNOWN_STEM"

  Scenario: a named process stem is skipped, nothing is pulled
    Given the fixture workspace "docker-mixed"
    When I run "stems pull echo-svc --json" with env DOCKER_HOST=unix:///nonexistent
    Then the command succeeds
    And the JSON at "$.data.pulled" equals []
    And the JSON at "$.data.skipped[0].stem" equals "echo-svc"
    And the JSON at "$.data.skipped[0].reason" equals "not a docker stem"
    And the lock file and socket do not exist

  @error
  Scenario: without Docker the pull fails and the daemon is not left behind
    Given the fixture workspace "docker-mixed"
    When I run "stems pull --json" with env DOCKER_HOST=unix:///nonexistent
    Then the exit code is 1
    And the JSON error has code "DOCKER_UNAVAILABLE"
    And the JSON at "$.data.failed[0].stem" equals "db"
    And the lock file and socket do not exist

@FR-CL-3 @error
Feature: Docker unavailable (the half that runs without Docker)
  With Docker unreachable, process-only selections still start; a selection
  that needs a docker stem fails with DOCKER_UNAVAILABLE before anything
  (even the daemon) starts, and doctor fails `docker.reachable`.

  Scenario: up of a process stem works, up of a docker stem does not
    Given the fixture workspace "docker-mixed"
    When I run "stems up db --detach --json" with env DOCKER_HOST=unix:///nonexistent
    Then the exit code is 1
    And the JSON error has code "DOCKER_UNAVAILABLE"
    And the JSON at "$.errors[0].hint" contains "start Docker Desktop"
    And the JSON at "$.errors[0].details.stems" equals ["db"]
    And the lock file and socket do not exist
    When I run "stems up echo-svc --detach --json" with env DOCKER_HOST=unix:///nonexistent
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["echo-svc"]
    When I run "stems doctor --json" with env DOCKER_HOST=unix:///nonexistent
    Then the exit code is 1
    And the JSON at "$.data.checks[?@.id == 'docker.reachable'].status" equals "fail"
    And the JSON at "$.data.checks[?@.id == 'docker.reachable'].hint" contains "start Docker Desktop"
    And the JSON at "$.data.checks[?@.id == 'docker.version']" does not exist
    And the JSON at "$.data.checks[?@.id == 'ports.echo-svc.http'].status" equals "ok"
    And the JSON error has code "DOCKER_UNAVAILABLE"
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive

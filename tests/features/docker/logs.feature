@docker @FR-LG-1
Feature: a docker stem's output is captured like a process's
  The container's stdout/stderr (Docker's log stream) flow into the stem's
  log files and `stems logs`. Needs Docker: `make e2e-docker`.

  Scenario: postgres's own start-up line is in its logs
    Given the fixture workspace "docker-pg"
    When I run "stems up --detach --json"
    Then the command succeeds
    And within 60s the stem log file "db/current.log" contains "ready to accept connections"
    When I run "stems logs db --grep 'ready to accept' --json"
    Then the command succeeds
    And stdout contains "ready to accept connections"
    When I run "stems down --volumes --yes --json"
    Then the command succeeds
    And no container with label stems.workspace=docker-pg exists

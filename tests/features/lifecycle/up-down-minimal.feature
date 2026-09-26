@FR-LC-1 @FR-LC-2
Feature: up and down on the minimal workspace
  `stems up --detach` auto-starts the workspace daemon, starts every stem
  in dependency order and returns once they are ready; `stems down --all`
  stops them in reverse order and shuts the daemon down.

  @FR-HS-2
  Scenario: up --detach, status, down --all
    Given the "minimal" workspace
    When I run "stems up --detach --json"
    Then the command succeeds
    And the JSON at "$.data.ok" equals true
    And the JSON at "$.data.ready" equals ["echo-svc"]
    And the JSON at "$.data.failed" equals []
    When I run "stems status --json"
    Then the command succeeds
    And the JSON at "$.data.stems[0].name" equals "echo-svc"
    And the JSON at "$.data.stems[0].type" equals "process"
    And the JSON at "$.data.stems[0].state" equals "healthy"
    And the JSON at "$.data.stems[0].glyph" equals "healthy"
    And the JSON at "$.data.stems[0].pid" is greater than 1
    And the JSON at "$.data.stems[0].ports[0]" equals {"name": "http", "port": ${port:18090}, "auto": false}
    And the JSON at "$.data.stems[0].restarts" equals 0
    And the JSON at "$.data.summary.healthy" equals 1
    And the events stream contains {"kind": "stem.state", "stem": "echo-svc", "from": "stopped", "to": "starting"} before {"kind": "stem.state", "stem": "echo-svc", "from": "starting", "to": "healthy"}
    When I run "stems down --all --json"
    Then the command succeeds
    And the JSON at "$.data.stopped" equals ["echo-svc"]
    And the JSON at "$.data.daemon_stopping" equals true
    And the lock file and socket do not exist
    And no process from the workspace's process groups is alive

  Scenario: up twice is a no-op for running stems
    Given the "minimal" workspace is up
    When I run "stems status --json"
    And I save the JSON at "$.data.stems[0].pid" as "pid"
    And I run "stems up --detach --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["echo-svc"]
    When I run "stems status --json"
    Then the JSON at "$.data.stems[0].pid" equals ${var:pid}
    When I run "stems down --json"
    Then the command succeeds
    And the JSON at "$.data.stopped" equals ["echo-svc"]
    And the JSON at "$.data.daemon_stopping" equals true
    And within 2s the lock file and socket do not exist

  @error
  Scenario: down without a running daemon exits 4
    Given the "minimal" workspace
    When I run "stems down --json"
    Then the exit code is 4
    And the JSON error has code "DAEMON_NOT_RUNNING"

  @error
  Scenario: up of an unknown stem is a config error
    Given the "minimal" workspace
    When I run "stems up nope --detach --json"
    Then the exit code is 2
    And the JSON error has code "UNKNOWN_STEM"

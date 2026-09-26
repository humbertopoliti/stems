@FR-CR-2 @NFR-2 @recovery
Feature: A new daemon adopts the stems of a crashed one
  After `kill -9` of the daemon, `stems up` starts a new daemon that
  verifies each stem in state.json (pid and start time) and adopts the live
  ones instead of starting a second copy.

  Scenario: up after kill -9 adopts the running stem
    Given the "minimal" workspace is up in detached mode
    When I run "stems status --json"
    And I save the JSON at "$.data.stems[0].pid" as "pid"
    And the daemon is killed with SIGKILL
    And I run "stems up --detach --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["echo-svc"]
    And the JSON at "$.data.failed" equals []
    And the JSON at "$.errors" equals []
    And within 5s the events stream contains {"kind": "stem.adopted", "stem": "echo-svc", "data": {"pid": ${var:pid}}}
    And within 5s the events stream contains {"kind": "stem.state", "stem": "echo-svc", "from": "stopped", "to": "healthy", "reason": "adopted"}
    When I run "stems status --json"
    Then the JSON at "$.data.stems[0].pid" equals ${var:pid}
    And the JSON at "$.data.stems[0].state" equals "healthy"
    And the JSON at "$.data.stems[0].reason" equals "adopted"
    And the JSON at "$.data.stems[0].ports[0].port" equals ${port:18090}
    When I run "stems down --json"
    Then the command succeeds
    And the JSON at "$.data.stopped" equals ["echo-svc"]
    And within 2s the lock file and socket do not exist
    And no process from the workspace's process groups is alive
    And the state file contains no stems

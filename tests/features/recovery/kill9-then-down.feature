@FR-CR-2 @FR-CR-3 @FR-LC-5 @NFR-2 @recovery
Feature: Cleanup survives a daemon crash
  The daemon persists what it runs in state.json. After `kill -9` of the
  daemon, `stems down` starts a new daemon that adopts the live stems from
  the state file and stops them all, including forked children.

  @FR-CR-6
  Scenario: kill -9 on the daemon, then down cleans everything
    Given the "minimal" workspace is up in detached mode
    And the chaos endpoint "fork?n=3" is called on "echo-svc"
    Then the chaos response status is 200
    When the daemon is killed with SIGKILL
    And I run "stems status --json"
    Then the exit code is 4
    And the JSON error has code "DAEMON_NOT_RUNNING"
    And the JSON at "$.errors[0].hint" contains "stems from a previous run are still alive"
    And the JSON at "$.errors[0].hint" contains "run `stems down`"
    When I run "stems daemon status --json"
    Then the exit code is 4
    And the JSON at "$.data.lock_state" equals "stale"
    And the JSON at "$.data.state.stems" equals 1
    And the JSON at "$.data.state.alive" equals 1
    When I run "stems down --json"
    Then the command succeeds
    And the JSON at "$.data.recovered" equals true
    And the JSON at "$.data.stopped" equals ["echo-svc"]
    And the JSON at "$.data.daemon_stopping" equals true
    And no process from the workspace's process groups is alive
    And the state file contains no stems
    And the lock file and socket do not exist

  @error
  Scenario: down with neither a daemon nor a state file still exits 4
    Given the "minimal" workspace
    When I run "stems down --json"
    Then the exit code is 4
    And the JSON error has code "DAEMON_NOT_RUNNING"

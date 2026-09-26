@FR-CR-5 @recovery
Feature: A stale daemon lock is reclaimed
  A lock whose pid is dead (a crashed daemon) does not block the workspace:
  the CLI names it in its hint (FR-CR-6) and the next start reclaims it.

  @FR-CR-6
  Scenario: start reclaims a lock left by a dead process
    Given the "minimal" workspace
    And a stale daemon lock from a dead process and its socket
    When I run "stems daemon status --json"
    Then the exit code is 4
    And the JSON error has code "DAEMON_NOT_RUNNING"
    And the JSON at "$.errors[0].hint" contains "stale lock"
    And the JSON at "$.data.lock_state" equals "stale"
    When I run "stems daemon start --json"
    Then the command succeeds
    And the JSON at "$.data.already_running" equals false
    And within 5s the daemon log contains "stale lock reclaimed"
    When I run "stems daemon status --json"
    Then the command succeeds
    And the JSON at "$.data.running" equals true
    And the JSON at "$.data.lock_state" equals "held"

  Scenario: stop cleans up a stale lock
    Given the "minimal" workspace
    And a stale daemon lock from a dead process
    When I run "stems daemon stop --json"
    Then the command succeeds
    And the JSON at "$.data.stopped" equals false
    And the JSON at "$.data.stale_lock_removed" equals true
    And the lock file and socket do not exist

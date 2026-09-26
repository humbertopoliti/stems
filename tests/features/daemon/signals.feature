@FR-CR-5 @recovery
Feature: Daemon signals
  SIGTERM runs the same orderly shutdown as `stems daemon stop`; SIGKILL
  leaves a stale lock that the CLI detects and names (FR-CR-6).

  Scenario: SIGTERM removes the socket and the lock
    Given the "minimal" workspace
    And the daemon is started
    When the daemon is killed with SIGTERM
    Then within 2s the lock file and socket do not exist
    And within 2s the daemon log contains "signal received"

  @FR-CR-6 @error
  Scenario: SIGKILL leaves a stale lock that status reports
    Given the "minimal" workspace
    And the daemon is started
    When the daemon is killed with SIGKILL
    And I run "stems daemon status --json"
    Then the exit code is 4
    And the JSON error has code "DAEMON_NOT_RUNNING"
    And the JSON at "$.errors[0].hint" contains "stale lock"
    And the JSON at "$.data.lock_state" equals "stale"
    When I run "stems daemon start --json"
    Then the command succeeds
    And within 5s the daemon log contains "stale lock reclaimed"

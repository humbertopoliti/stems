@FR-DS-2
Feature: CLI and daemon versions must match
  A client refuses to drive a daemon of another stems version
  (DAEMON_VERSION_MISMATCH, exit 4) and says how to restart it; the restart
  it suggests (`stems daemon stop`) still works across versions.

  @error
  Scenario: a client of another version gets DAEMON_VERSION_MISMATCH
    Given the "minimal" workspace
    And the daemon is started
    When I run "stems events --json" with env STEMS_FAKE_VERSION=0.0.1
    Then the exit code is 4
    And the JSON error has code "DAEMON_VERSION_MISMATCH"
    And the JSON at "$.errors[0].hint" contains "restart the daemon"
    When I run "stems daemon start --json" with env STEMS_FAKE_VERSION=0.0.1
    Then the exit code is 4
    And the JSON error has code "DAEMON_VERSION_MISMATCH"
    When I run "stems daemon status --json" with env STEMS_FAKE_VERSION=0.0.1
    Then the exit code is 4
    And the JSON error has code "DAEMON_VERSION_MISMATCH"
    And the JSON at "$.data.running" equals true
    And the JSON at "$.data.compatible" equals false

  Scenario: stop works across versions, so the hint is actionable
    Given the "minimal" workspace
    And the daemon is started
    When I run "stems daemon stop --json" with env STEMS_FAKE_VERSION=0.0.1
    Then the command succeeds
    And the JSON at "$.data.stopped" equals true
    And the lock file and socket do not exist

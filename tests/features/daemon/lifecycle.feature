@FR-CR-5
Feature: Daemon lifecycle
  One daemon per workspace (FR-CR-5), started detached by `stems daemon
  start`, inspected with `stems daemon status`, stopped with `stems daemon
  stop`. It listens on a Unix socket with mode 0600 (NFR-4) and answers the
  CLI quickly (NFR-1).

  @error
  Scenario: status without a running daemon is DAEMON_NOT_RUNNING
    Given the "minimal" workspace
    When I run "stems daemon status --json"
    Then the exit code is 4
    And the JSON error has code "DAEMON_NOT_RUNNING"
    And the JSON at "$.data.running" equals false
    And the JSON at "$.data.lock_state" equals "free"

  @error
  Scenario: stop and events without a running daemon exit 4
    Given the "minimal" workspace
    When I run "stems daemon stop --json"
    Then the exit code is 4
    And the JSON error has code "DAEMON_NOT_RUNNING"
    When I run "stems events --json"
    Then the exit code is 4
    And the JSON error has code "DAEMON_NOT_RUNNING"

  @NFR-1 @NFR-4
  Scenario: start, status, start again, stop
    Given the "minimal" workspace
    When I run "stems daemon start --json"
    Then the command succeeds
    And the JSON at "$.data.already_running" equals false
    And the JSON at "$.data.pid" exists
    And the socket has mode 0600
    When I run "stems daemon status --json"
    Then the command succeeds
    And the last command took less than 100 ms
    And the JSON at "$.data.running" equals true
    And the JSON at "$.data.pid" exists
    And the JSON at "$.data.version" matches semver
    And the JSON at "$.data.api_version" equals 1
    And the JSON at "$.data.workspace" equals "${ws}"
    And the JSON at "$.data.socket" contains "stemsd.sock"
    When I run "stems daemon start --json"
    Then the command succeeds
    And the JSON at "$.data.already_running" equals true
    When I run "stems daemon stop --json"
    Then the command succeeds
    And the JSON at "$.data.stopped" equals true
    And the lock file and socket do not exist

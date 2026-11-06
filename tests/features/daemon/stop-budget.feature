@FR-CR-5
Feature: `stems daemon stop` waits as long as the daemon's shutdown may take
  The daemon stops every running stem before it exits. Its shutdown budget is
  `daemon.shutdown_timeout`, by default 30s plus the longest stop of each
  stop layer (`pre_stop` timeout + `stop_grace` + `post_stop` timeout), and
  the `shutdown` reply carries it, so `stems daemon stop` waits that long
  plus 5s. `--timeout` replaces the wait, `--no-wait` does not wait; a daemon
  still stopping when the wait ends is DAEMON_STILL_STOPPING, not INTERNAL.

  Scenario: --no-wait returns at once with the default budget
    Given the fixture workspace "slow-stop"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems daemon stop --no-wait --json"
    Then the command succeeds
    And the last command took less than 2000 ms
    And the JSON at "$.data.stopping" equals true
    And the JSON at "$.data.stopped" equals false
    And the JSON at "$.data.budget_ms" equals 120000
    And within 15s the lock file and socket do not exist
    And no process from the workspace's process groups is alive

  @error
  Scenario: a wait shorter than the stems' stop is DAEMON_STILL_STOPPING, and stop again keeps waiting
    Given the fixture workspace "slow-stop"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems daemon stop --timeout 1s --json"
    Then the exit code is 1
    And the JSON error has code "DAEMON_STILL_STOPPING"
    And the error message contains "is still stopping after 1s"
    And the JSON at "$.errors[0].hint" contains "keeps shutting down in the background"
    And the JSON at "$.errors[0].hint" contains "stemsd.log"
    And the JSON at "$.errors[0].hint" contains "stems daemon stop"
    When I run "stems daemon stop --json"
    Then the command succeeds
    And the JSON at "$.data.stopped" equals true
    And the lock file and socket do not exist
    And no process from the workspace's process groups is alive

  Scenario: daemon.shutdown_timeout sets the budget
    Given the fixture workspace "slow-stop"
    And a local override setting daemon.shutdown_timeout=20s
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems daemon stop --json"
    Then the command succeeds
    And the JSON at "$.data.stopped" equals true
    And the JSON at "$.data.budget_ms" equals 20000
    And the last command took at least 3000 ms
    And the lock file and socket do not exist

  @error
  Scenario: --timeout takes a duration
    Given the fixture workspace "slow-stop"
    When I run "stems daemon stop --timeout soon --json"
    Then the exit code is 2
    And the JSON error has code "USAGE"

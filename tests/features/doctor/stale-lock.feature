@FR-CL-3 @recovery
Feature: doctor finds and removes a stale daemon lock
  A lock whose pid is dead (a crashed daemon) is a fixable failure;
  `--fix` needs consent (`--yes` or the prompt) and then removes the lock
  and the socket nobody listens on.

  Scenario: dead-pid lock and socket
    Given the "minimal" workspace
    And a stale daemon lock from a dead process and its socket
    When I run "stems doctor --json"
    Then the exit code is 1
    And the JSON at "$.data.checks[?@.id == 'daemon'].status" equals "fail"
    And the JSON at "$.data.checks[?@.id == 'daemon'].fixable" equals true
    And the JSON at "$.data.checks[?@.id == 'daemon'].details.lock_state" equals "stale"
    And the JSON error has code "DAEMON_NOT_RUNNING"
    When I run "stems doctor --fix --json"
    Then the exit code is 1
    And the JSON at "$.data.fix_skipped" contains "--yes"
    And the JSON at "$.data.fixed" equals []
    When I run "stems doctor --fix --yes --json"
    Then the command succeeds
    And the JSON at "$.data.ok" equals true
    And the JSON at "$.data.fixed[?@.action == 'remove_stale_lock'].ok" equals true
    And the JSON at "$.data.fixed[?@.action == 'remove_stale_socket'].ok" equals true
    And the JSON at "$.data.checks[?@.id == 'daemon'].status" equals "ok"
    And the lock file and socket do not exist
    When I run "stems daemon status --json"
    Then the exit code is 4
    And the JSON at "$.data.lock_state" equals "free"

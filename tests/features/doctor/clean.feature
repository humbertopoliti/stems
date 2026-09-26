@FR-CL-3
Feature: stems doctor on a machine that is ready
  `stems doctor` runs every check without a daemon (a running daemon is only
  asked for its health) and exits 0 when nothing fails.

  Scenario: the minimal workspace is all ok
    Given the "minimal" workspace
    When I run "stems doctor --json"
    Then the command succeeds
    And the JSON at "$.data.ok" equals true
    And the JSON at "$.data.summary.fail" equals 0
    And the JSON at "$.data.summary.warn" equals 0
    And the JSON at "$.data.checks[?@.id == 'config'].status" equals "ok"
    And the JSON at "$.data.checks[?@.id == 'daemon'].status" equals "ok"
    And the JSON at "$.data.checks[?@.id == 'daemon'].details.running" equals false
    And the JSON at "$.data.checks[?@.id == 'ports.echo-svc.http'].status" equals "ok"
    And the JSON at "$.data.checks[?@.id == 'codebase.echo-svc'].status" equals "ok"
    And the JSON at "$.data.checks[?@.id == 'orphans'].status" equals "ok"
    And the JSON at "$.data.checks[?@.id == 'disk.home'].status" equals "ok"
    And the JSON at "$.data.checks[?@.id == 'docker.reachable']" does not exist
    And the JSON at "$.data.fixed" equals []
    And the lock file and socket do not exist

  Scenario: with the stems running, the daemon's health and its ports are ok
    Given the "minimal" workspace is up in detached mode
    When I run "stems doctor --json"
    Then the command succeeds
    And the JSON at "$.data.ok" equals true
    And the JSON at "$.data.checks[?@.id == 'daemon'].details.running" equals true
    And the JSON at "$.data.checks[?@.id == 'daemon'].details.compatible" equals true
    And the JSON at "$.data.checks[?@.id == 'ports.echo-svc.http'].status" equals "ok"
    And the JSON at "$.data.checks[?@.id == 'ports.echo-svc.http'].details.stem" equals "echo-svc"
    And the JSON at "$.data.checks[?@.id == 'orphans'].status" equals "ok"
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive

  Scenario: the human report is a table with a summary
    Given the "minimal" workspace
    When I run "stems doctor --human"
    Then the command succeeds
    And stdout contains "CHECK"
    And stdout contains "ports.echo-svc.http"
    And stdout contains "0 fail"

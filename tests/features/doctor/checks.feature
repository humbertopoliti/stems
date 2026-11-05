@FR-CL-3
Feature: user-defined `checks:` in doctor and before `up`
  Fixture checks-demo declares shell checks (exit 0 = ok): `fine`,
  `needs-marker` (fails until its `fix` creates marker.txt), `soft`
  (severity warn), `slow` (sleep 5 with a 1s timeout) and `doctor-only`
  (`when: [doctor]`), and stem `web` has `in-codebase` (runs in the codebase
  with the stem's env). The disabled stem's check is not run.

  Scenario: doctor reports each custom check with its own status
    Given the fixture workspace "checks-demo"
    When I run "stems doctor --json"
    Then the exit code is 1
    And the JSON at "$.data.checks[?@.id == 'check.fine'].status" equals "ok"
    And the JSON at "$.data.checks[?@.id == 'check.needs-marker'].status" equals "fail"
    And the JSON at "$.data.checks[?@.id == 'check.needs-marker'].message" equals "marker.txt is missing"
    And the JSON at "$.data.checks[?@.id == 'check.needs-marker'].hint" equals "create marker.txt"
    And the JSON at "$.data.checks[?@.id == 'check.needs-marker'].fixable" equals true
    And the JSON at "$.data.checks[?@.id == 'check.needs-marker'].details.exit" equals 1
    And the JSON at "$.data.checks[?@.id == 'check.soft'].status" equals "warn"
    And the JSON at "$.data.checks[?@.id == 'check.soft'].details.exit" equals 3
    And the JSON at "$.data.checks[?@.id == 'check.soft'].details.tail" contains "noisy-output"
    And the JSON at "$.data.checks[?@.id == 'check.slow'].status" equals "fail"
    And the JSON at "$.data.checks[?@.id == 'check.slow'].message" contains "timed out"
    And the JSON at "$.data.checks[?@.id == 'check.web.in-codebase'].status" equals "ok"
    And the JSON at "$.data.checks[?@.id == 'check.off.never']" does not exist

  Scenario: doctor --fix runs the fix command and the check turns ok
    Given the fixture workspace "checks-demo"
    When I run "stems doctor --fix --yes --json"
    Then the JSON at "$.data.fixed[?@.action == 'run_fix'].id" equals "check.needs-marker"
    And the JSON at "$.data.fixed[?@.action == 'run_fix'].ok" equals true
    And the JSON at "$.data.checks[?@.id == 'check.needs-marker'].status" equals "ok"
    And the file "marker.txt" exists

  Scenario: a failing fail-severity check aborts up before anything starts
    Given the fixture workspace "checks-demo"
    When I run "stems up --detach --json"
    Then the exit code is 1
    And the JSON error has code "CHECK_FAILED"
    And the JSON at "$.errors[0].details.check" equals "check.needs-marker"
    And the JSON at "$.errors[0].details.exit" equals 1
    And the JSON at "$.errors[0].hint" equals "create marker.txt"
    And the lock file and socket do not exist

  Scenario: once the check passes, up goes ahead; doctor-only checks are not run by up
    Given the fixture workspace "checks-demo"
    And the file "marker.txt" is written with "x"
    When I run "stems up --detach --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["web"]
    When I run "stems down --json"
    Then the command succeeds

@FR-CL-3
Feature: doctor reports a foreign process on a declared port
  A process stems did not start that listens on a declared port is a
  warning with its pid and command. It is not fixable: `--fix --yes` leaves
  it alone (only `--kill-foreign` would kill it).

  Scenario: python -m http.server on echo-svc's port
    Given the "minimal" workspace
    And a stray process "cd ${tmp} && exec python3 -m http.server ${port:18090} --bind 127.0.0.1" is running in a new process group
    And within 10s port ${port:18090} is listening
    When I run "stems doctor --json"
    Then the command succeeds
    And the JSON at "$.data.ok" equals true
    And the JSON at "$.data.checks[?@.id == 'ports.echo-svc.http'].status" equals "warn"
    And the JSON at "$.data.checks[?@.id == 'ports.echo-svc.http'].fixable" equals false
    And the JSON at "$.data.checks[?@.id == 'ports.echo-svc.http'].details.pid" exists
    And the JSON at "$.data.checks[?@.id == 'ports.echo-svc.http'].details.command" contains "http.server"
    And the JSON at "$.data.checks[?@.id == 'ports.echo-svc.http'].details.port" equals ${port:18090}
    And the JSON at "$.data.checks[?@.id == 'orphans'].status" equals "warn"
    And the JSON at "$.data.checks[?@.id == 'orphans'].fixable" equals false
    When I run "stems doctor --strict --json"
    Then the exit code is 1
    And the JSON at "$.data.ok" equals false
    When I run "stems doctor --fix --yes --json"
    Then the command succeeds
    And the JSON at "$.data.fixed" equals []
    And the stray processes are still running
    When the stray processes are stopped
    And I run "stems doctor --json"
    Then the command succeeds
    And the JSON at "$.data.checks[?@.id == 'ports.echo-svc.http'].status" equals "ok"

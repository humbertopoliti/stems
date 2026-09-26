@FR-CL-1
Feature: JSON envelope and TTY detection
  Every command prints `{ "ok", "data", "errors", "version" }` when stdout is
  not a terminal (the harness always pipes stdout) or with `--json`; `--human`
  forces text. Command-line errors are `USAGE` (exit 2) in the same envelope.

  Scenario: validate on a pipe emits the envelope without --json
    Given the "minimal" workspace
    When I run "stems validate"
    Then the command succeeds
    And the JSON at "$.ok" equals true
    And the JSON at "$.data.ok" equals true
    And the JSON at "$.data.warnings" equals []
    And the JSON at "$.errors" equals []
    And the JSON at "$.version" matches semver

  Scenario: --human forces text on a pipe
    Given the "minimal" workspace
    When I run "stems validate --human"
    Then the command succeeds
    And stdout is not JSON
    And stdout contains "valid"

  @error
  Scenario: an unknown subcommand is a USAGE error in JSON
    When I run "stems frobnicate"
    Then the exit code is 2
    And the JSON at "$.ok" equals false
    And the JSON at "$.data" equals null
    And the JSON error has code "USAGE"
    And the error message contains "frobnicate"

  @error
  Scenario: an unknown flag is a USAGE error in JSON
    Given the "minimal" workspace
    When I run "stems validate --no-such-flag"
    Then the exit code is 2
    And the JSON error has code "USAGE"

  @error
  Scenario: config errors keep the envelope and exit 2
    Given the broken workspace "cycle"
    When I run "stems validate"
    Then the exit code is 2
    And the JSON at "$.ok" equals false
    And the JSON at "$.data.ok" equals false
    And the JSON error has code "CYCLE"

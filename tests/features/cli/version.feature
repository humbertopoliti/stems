@FR-DS-2
Feature: stems --version
  `stems --version` prints `stems <semver>`; with `--json` it prints the
  envelope with `data: { version, commit, build_date }` and the top-level
  `version`.

  Scenario: --version --json reports version, commit and build date
    When I run "stems --version --json"
    Then the command succeeds
    And the JSON at "$.version" matches semver
    And the JSON at "$.data.version" matches semver
    And the JSON at "$.data.commit" exists
    And the JSON at "$.data.build_date" exists
    And the JSON at "$.errors" equals []

  Scenario: --version without --json prints plain text even on a pipe
    When I run "stems --version"
    Then the command succeeds
    And stdout contains "stems 0."
    And stdout is not JSON
